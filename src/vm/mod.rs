use core::{
    fmt::Write,
    mem::size_of,
    ops::{Deref, Neg},
    ptr::NonNull,
    str::FromStr,
};

use crate::{
    dictionary::{
        BuiltinEntry, BumpError, DictionaryBump, DictionaryEntry, EntryHeader, EntryKind,
    },
    fastr::{FaStr, TmpFaStr},
    input::WordStrBuf,
    output::{OutputBuf, OutputError},
    stack::Stack,
    word::Word,
    CallContext, Error, Lookup, Mode, ReplaceErr, WordFunc,
};

pub mod builtins;

/// Forth is the "context" of the VM/interpreter.
///
/// It does NOT include the input/output buffers, or any components that
/// directly rely on those buffers. This Forth context is composed with
/// the I/O buffers to create the `Fif` type. This is done for lifetime
/// reasons.
pub struct Forth<T: 'static> {
    mode: Mode,
    pub(crate) data_stack: Stack<Word>,
    return_stack: Stack<Word>,
    call_stack: Stack<CallContext<T>>,
    pub(crate) dict_alloc: DictionaryBump,
    run_dict_tail: Option<NonNull<DictionaryEntry<T>>>,
    pub input: WordStrBuf,
    pub output: OutputBuf,
    pub host_ctxt: T,
    builtins: &'static [BuiltinEntry<T>],

    // TODO: This will be for words that have compile time actions, I guess?
    _comp_dict_tail: Option<NonNull<DictionaryEntry<T>>>,
}

impl<T> Forth<T> {
    pub unsafe fn new(
        dstack_buf: (*mut Word, usize),
        rstack_buf: (*mut Word, usize),
        cstack_buf: (*mut CallContext<T>, usize),
        dict_buf: (*mut u8, usize),
        input: WordStrBuf,
        output: OutputBuf,
        host_ctxt: T,
        builtins: &'static [BuiltinEntry<T>],
    ) -> Result<Self, Error> {
        let data_stack = Stack::new(dstack_buf.0, dstack_buf.1);
        let return_stack = Stack::new(rstack_buf.0, rstack_buf.1);
        let call_stack = Stack::new(cstack_buf.0, cstack_buf.1);
        let dict_alloc = DictionaryBump::new(dict_buf.0, dict_buf.1);

        Ok(Self {
            mode: Mode::Run,
            data_stack,
            return_stack,
            call_stack,
            dict_alloc,
            run_dict_tail: None,
            _comp_dict_tail: None,
            input,
            output,
            host_ctxt,
            builtins,
        })
    }

    pub fn add_builtin_static_name(
        &mut self,
        name: &'static str,
        bi: WordFunc<T>,
    ) -> Result<(), Error> {
        let name = unsafe { FaStr::new(name.as_ptr(), name.len()) };
        self.add_bi_fastr(name, bi)
    }

    pub fn add_builtin(&mut self, name: &str, bi: WordFunc<T>) -> Result<(), Error> {
        let name = self.dict_alloc.bump_str(name)?;
        self.add_bi_fastr(name, bi)
    }

    fn add_bi_fastr(&mut self, name: FaStr, bi: WordFunc<T>) -> Result<(), Error> {
        // Allocate and initialize the dictionary entry
        let dict_base = self.dict_alloc.bump::<DictionaryEntry<T>>()?;
        unsafe {
            dict_base.as_ptr().write(DictionaryEntry {
                hdr: EntryHeader {
                    func: bi,
                    name,
                    kind: EntryKind::RuntimeBuiltin,
                    len: 0,
                },
                link: self.run_dict_tail.take(),
                parameter_field: [],
            });
        }
        self.run_dict_tail = Some(dict_base);
        Ok(())
    }

    fn parse_num(word: &str) -> Option<i32> {
        i32::from_str(word).ok()
    }

    fn find_word(&self, word: &str) -> Option<NonNull<EntryHeader<T>>> {
        let fastr = TmpFaStr::new_from(word);
        self.find_in_dict(&fastr)
            .map(NonNull::cast)
            .or_else(|| self.find_in_bis(&fastr).map(NonNull::cast))
    }

    fn find_in_bis(&self, fastr: &TmpFaStr<'_>) -> Option<NonNull<BuiltinEntry<T>>> {
        self.builtins
            .iter()
            .find(|bi| &bi.hdr.name == fastr.deref())
            .map(NonNull::from)
    }

    fn find_in_dict(&self, fastr: &TmpFaStr<'_>) -> Option<NonNull<DictionaryEntry<T>>> {
        let mut optr: Option<NonNull<DictionaryEntry<T>>> = self.run_dict_tail;
        while let Some(ptr) = optr.take() {
            let de = unsafe { ptr.as_ref() };
            if &de.hdr.name == fastr.deref() {
                return Some(ptr);
            }
            optr = de.link;
        }
        None
    }

    pub fn lookup(&self, word: &str) -> Result<Lookup<T>, Error> {
        match word {
            ";" => Ok(Lookup::Semicolon),
            "if" => Ok(Lookup::If),
            "else" => Ok(Lookup::Else),
            "then" => Ok(Lookup::Then),
            "do" => Ok(Lookup::Do),
            "loop" => Ok(Lookup::Loop),
            "(" => Ok(Lookup::LParen),
            r#".""# => Ok(Lookup::LQuote),
            _ => {
                let fastr = TmpFaStr::new_from(word);
                if let Some(entry) = self.find_in_dict(&fastr) {
                    Ok(Lookup::Dict { de: entry })
                } else if let Some(bis) = self.find_in_bis(&fastr) {
                    Ok(Lookup::Builtin { bi: bis })
                } else if let Some(val) = Self::parse_num(word) {
                    Ok(Lookup::Literal { val })
                } else {
                    Err(Error::LookupFailed)
                }
            }
        }
    }

    pub fn process_line(&mut self) -> Result<(), Error> {
        loop {
            self.input.advance();
            let word = match self.input.cur_word() {
                Some(w) => w,
                None => break,
            };

            match self.lookup(word)? {
                Lookup::Dict { de } => {
                    let dref = unsafe { de.as_ref() };
                    self.call_stack.push(CallContext {
                        eh: de.cast(),
                        idx: 0,
                        len: dref.hdr.len,
                    })?;
                    let res = (dref.hdr.func)(self);
                    self.call_stack.pop().ok_or(Error::CallStackCorrupted)?;
                    res?;
                }
                Lookup::Builtin { bi } => {
                    // TODO: Do I want to push builtins to the call stack?
                    self.call_stack.push(CallContext {
                        eh: bi.cast(),
                        idx: 0,
                        len: 0,
                    })?;
                    let res = unsafe { (bi.as_ref().hdr.func)(self) };
                    self.call_stack.pop().ok_or(Error::CallStackCorrupted)?;
                    res?;
                }
                Lookup::Literal { val } => {
                    self.data_stack.push(Word::data(val))?;
                }
                Lookup::LParen => {
                    self.munch_comment(&mut 0)?;
                }
                Lookup::Semicolon => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::If => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::Else => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::Then => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::Do => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::Loop => return Err(Error::InterpretingCompileOnlyWord),
                Lookup::LQuote => {
                    self.input.advance_str().replace_err(Error::BadStrLiteral)?;
                    let lit = self.input.cur_str_literal().unwrap();
                    self.output.push_str(lit)?;
                }
            }
        }
        writeln!(&mut self.output, "ok.").map_err(|_| OutputError::FormattingErr)?;
        Ok(())
    }

    fn munch_do(&mut self, len: &mut u16) -> Result<u16, Error> {
        let start = *len;

        // Write a conditional jump, followed by space for a literal
        let literal_cj = self.find_word("2d>2r").ok_or(Error::WordNotInDict)?;
        self.dict_alloc.bump_write(Word::ptr(literal_cj.as_ptr()))?;
        *len += 1;

        let do_start = *len;
        // Now work until we hit an else or then statement.
        loop {
            match self.munch_one(len) {
                // We hit the end of stream before an else/then.
                Ok(0) => return Err(Error::DoWithoutLoop),
                // We compiled some stuff, keep going...
                Ok(_) => {}
                Err(Error::LoopBeforeDo) => {
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        let delta = *len - do_start;
        let offset = i32::from(delta + 1).neg();
        let literal_dojmp = self.find_word("(jmp-doloop)").ok_or(Error::WordNotInDict)?;
        self.dict_alloc
            .bump_write(Word::ptr(literal_dojmp.as_ptr()))?;
        self.dict_alloc.bump_write(Word::data(offset))?;
        *len += 2;

        Ok(*len - start)
    }

    fn munch_if(&mut self, len: &mut u16) -> Result<u16, Error> {
        let start = *len;

        // Write a conditional jump, followed by space for a literal
        let literal_cj = self.find_word("(jump-zero)").ok_or(Error::WordNotInDict)?;
        self.dict_alloc.bump_write(Word::ptr(literal_cj.as_ptr()))?;
        let cj_offset: &mut i32 = {
            let cj_offset_word = self.dict_alloc.bump::<Word>()?;
            unsafe {
                cj_offset_word.as_ptr().write(Word::data(0));
                &mut (*cj_offset_word.as_ptr()).data
            }
        };

        // Increment the length for the number so far.
        *len += 2;

        let mut else_then = false;
        let if_start = *len;
        // Now work until we hit an else or then statement.
        loop {
            match self.munch_one(len) {
                // We hit the end of stream before an else/then.
                Ok(0) => return Err(Error::IfWithoutThen),
                // We compiled some stuff, keep going...
                Ok(_) => {}
                Err(Error::ElseBeforeIf) => {
                    else_then = true;
                    break;
                }
                Err(Error::ThenBeforeIf) => break,
                Err(e) => return Err(e),
            }
        }

        let delta = *len - if_start;
        if !else_then {
            // we got a "then"
            //
            // Jump offset is words placed + 1 for the jump-zero literal
            *cj_offset = i32::from(delta) + 1;
            return Ok(*len - start);
        }
        // We got an "else", keep going for "then"
        //
        // Jump offset is words placed + 1 (cj lit) + 2 (else cj + lit)
        *cj_offset = i32::from(delta) + 3;

        // Write a conditional jump, followed by space for a literal
        let literal_jmp = self.find_word("(jmp)").ok_or(Error::WordNotInDict)?;
        self.dict_alloc
            .bump_write(Word::ptr(literal_jmp.as_ptr()))?;
        let jmp_offset: &mut i32 = {
            let jmp_offset_word = self.dict_alloc.bump::<Word>()?;
            unsafe {
                jmp_offset_word.as_ptr().write(Word::data(0));
                &mut (*jmp_offset_word.as_ptr()).data
            }
        };
        *len += 2;

        let else_start = *len;
        // Now work until we hit a then statement.
        loop {
            match self.munch_one(len) {
                // We hit the end of stream before a then.
                Ok(0) => return Err(Error::IfElseWithoutThen),
                // We compiled some stuff, keep going...
                Ok(_) => {}
                Err(Error::ElseBeforeIf) => return Err(Error::DuplicateElse),
                Err(Error::ThenBeforeIf) => break,
                Err(e) => return Err(e),
            }
        }

        let delta = *len - else_start;
        // Jump offset is words placed + 1 (jmp lit)
        *jmp_offset = i32::from(delta) + 1;

        Ok(*len - start)
    }

    fn munch_one(&mut self, len: &mut u16) -> Result<u16, Error> {
        let start = *len;
        self.input.advance();
        let word = match self.input.cur_word() {
            Some(w) => w,
            None => return Ok(0),
        };

        match self.lookup(word)? {
            Lookup::If => return self.munch_if(len),
            Lookup::Else => return Err(Error::ElseBeforeIf),
            Lookup::Then => return Err(Error::ThenBeforeIf),
            Lookup::Semicolon => return Ok(0),
            Lookup::Dict { de } => {
                // Dictionary items are put into the CFA array directly as
                // a pointer to the dictionary entry
                self.dict_alloc.bump_write(Word::ptr(de.as_ptr()))?;
                *len += 1;
            }
            Lookup::Builtin { bi } => {
                self.dict_alloc.bump_write(Word::ptr(bi.as_ptr()))?;
                *len += 1;
            }
            Lookup::Literal { val } => {
                // Literals are added to the CFA as two items:
                //
                // 1. The address of the `literal()` dictionary item
                // 2. The value of the literal, as a data word
                let literal_dict = self.find_word("(literal)").ok_or(Error::WordNotInDict)?;
                self.dict_alloc
                    .bump_write(Word::ptr(literal_dict.as_ptr()))?;
                self.dict_alloc.bump_write(Word::data(val))?;
                *len += 2;
            }
            Lookup::Do => return self.munch_do(len),
            Lookup::Loop => return Err(Error::LoopBeforeDo),
            Lookup::LParen => return self.munch_comment(len),
            Lookup::LQuote => return self.munch_str(len),
        }
        Ok(*len - start)
    }

    pub fn release(self) -> T {
        self.host_ctxt
    }

    fn munch_comment(&mut self, _len: &mut u16) -> Result<u16, Error> {
        loop {
            self.input.advance();
            match self.input.cur_word() {
                Some(s) => {
                    if s.ends_with(')') {
                        return Ok(0);
                    }
                }
                None => return Ok(0),
            }
        }
    }

    fn munch_str(&mut self, len: &mut u16) -> Result<u16, Error> {
        let start = *len;
        self.input
            .advance_str()
            .replace_err(Error::LQuoteMissingRQuote)?;
        let lit_str = self
            .input
            .cur_str_literal()
            .ok_or(Error::LQuoteMissingRQuote)?;
        let str_len =
            u16::try_from(lit_str.as_bytes().len()).replace_err(Error::LiteralStringTooLong)?;

        let literal_writestr = self.find_word("(write-str)").ok_or(Error::WordNotInDict)?;
        self.dict_alloc
            .bump_write::<Word>(Word::ptr(literal_writestr.as_ptr()))?;
        self.dict_alloc
            .bump_write::<Word>(Word::data(str_len.into()))?;
        *len += 2;

        let start_ptr = self
            .dict_alloc
            .bump_u8s(lit_str.as_bytes().len())
            .ok_or(Error::Bump(BumpError::OutOfMemory))?;

        unsafe {
            start_ptr
                .as_ptr()
                .copy_from_nonoverlapping(lit_str.as_bytes().as_ptr(), lit_str.as_bytes().len());
        }
        let word_size = size_of::<Word>();
        let words_written = (str_len as usize + (word_size - 1)) / word_size;
        *len += words_written as u16;

        Ok(*len - start)
    }
}