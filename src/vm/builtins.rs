use core::{fmt::Write, mem::size_of, ops::Neg, ptr::NonNull};

use crate::{
    dictionary::{BuiltinEntry, DictionaryEntry, EntryHeader, EntryKind},
    fastr::comptime_fastr,
    output::OutputError,
    vm::TmpFaStr,
    word::Word,
    CallContext, Error, Forth, Mode, ReplaceErr,
};

// NOTE: This macro exists because we can't have const constructors that include
// "mut" items, which unfortunately covers things like `fn(&mut T)`. Use a macro
// until this is resolved.
macro_rules! builtin {
    ($name:literal, $func:expr) => {
        BuiltinEntry {
            hdr: EntryHeader {
                name: comptime_fastr($name),
                func: $func,
                kind: EntryKind::StaticBuiltin,
                len: 0,
            },
        }
    };
}

// let literal_dict = self.find_word("(literal)").ok_or(Error::WordNotInDict)?;

impl<T: 'static> Forth<T> {
    pub const FULL_BUILTINS: &'static [BuiltinEntry<T>] = &[
        //
        // Math operations
        //
        builtin!("+", Self::add),
        builtin!("-", Self::minus),
        builtin!("/", Self::div),
        builtin!("mod", Self::modu),
        builtin!("/mod", Self::div_mod),
        builtin!("*", Self::mul),
        builtin!("abs", Self::abs),
        builtin!("negate", Self::negate),
        builtin!("min", Self::min),
        builtin!("max", Self::max),
        //
        // Floating Math operations
        //
        builtin!("f+", Self::float_add),
        builtin!("f-", Self::float_minus),
        builtin!("f/", Self::float_div),
        builtin!("fmod", Self::float_modu),
        builtin!("f/mod", Self::float_div_mod),
        builtin!("f*", Self::float_mul),
        builtin!("fabs", Self::float_abs),
        builtin!("fnegate", Self::float_negate),
        builtin!("fmin", Self::float_min),
        builtin!("fmax", Self::float_max),
        //
        // Double intermediate math operations
        //
        builtin!("*/", Self::star_slash),
        builtin!("*/mod", Self::star_slash_mod),
        //
        // Logic operations
        //
        builtin!("not", Self::invert),
        // NOTE! This is `bitand`, not logical `and`! e.g. `&` not `&&`.
        builtin!("and", Self::and),
        builtin!("=", Self::equal),
        builtin!(">", Self::greater),
        builtin!("<", Self::less),
        builtin!("0=", Self::zero_equal),
        builtin!("0>", Self::zero_greater),
        builtin!("0<", Self::zero_less),
        //
        // Stack operations
        //
        builtin!("swap", Self::swap),
        builtin!("dup", Self::dup),
        builtin!("over", Self::over),
        builtin!("rot", Self::rot),
        builtin!("drop", Self::ds_drop),
        //
        // Double operations
        //
        builtin!("2swap", Self::swap_2),
        builtin!("2dup", Self::dup_2),
        builtin!("2over", Self::over_2),
        builtin!("2drop", Self::ds_drop_2),
        //
        // String/Output operations
        //
        builtin!("emit", Self::emit),
        builtin!("cr", Self::cr),
        builtin!("space", Self::space),
        builtin!("spaces", Self::spaces),
        builtin!(".", Self::pop_print),
        builtin!("u.", Self::unsigned_pop_print),
        builtin!("f.", Self::float_pop_print),
        //
        // Define/forget
        //
        builtin!(":", Self::colon),
        builtin!("forget", Self::forget),
        //
        // Stack/Retstack operations
        //
        builtin!("d>r", Self::data_to_return_stack),
        // NOTE: REQUIRED for `do/loop`
        builtin!("2d>2r", Self::data2_to_return2_stack),
        builtin!("r>d", Self::return_to_data_stack),
        //
        // Loop operations
        //
        builtin!("i", Self::loop_i),
        builtin!("i'", Self::loop_itick),
        builtin!("j", Self::loop_j),
        builtin!("leave", Self::loop_leave),
        //
        // Memory operations
        //
        builtin!("@", Self::var_load),
        builtin!("!", Self::var_store),
        builtin!("w+", Self::word_add),
        //
        // Constants
        //
        builtin!("0", Self::zero_const),
        builtin!("1", Self::one_const),
        //
        // Other
        //
        // NOTE: REQUIRED for `."`
        builtin!("(write-str)", Self::write_str_lit),
        // NOTE: REQUIRED for `do/loop`
        builtin!("(jmp-doloop)", Self::jump_doloop),
        // NOTE: REQUIRED for `if/then` and `if/else/then`
        builtin!("(jump-zero)", Self::jump_if_zero),
        // NOTE: REQUIRED for `if/else/then`
        builtin!("(jmp)", Self::jump),
        // NOTE: REQUIRED for `:` (if you want literals)
        builtin!("(literal)", Self::literal),
        // NOTE: REQUIRED for `constant`
        builtin!("(constant)", Self::constant),
        // NOTE: REQUIRED for `variable` or `array`
        builtin!("(variable)", Self::variable),
    ];

    // addr offset w+
    pub fn word_add(&mut self) -> Result<(), Error> {
        let w_offset = self.data_stack.try_pop()?;
        let w_addr = self.data_stack.try_pop()?;
        let new_addr = unsafe {
            let offset = isize::try_from(w_offset.data).replace_err(Error::BadWordOffset)?;
            w_addr.ptr.cast::<Word>().offset(offset)
        };
        self.data_stack.push(Word::ptr(new_addr))?;
        Ok(())
    }

    // TODO: Check alignment?
    pub fn var_load(&mut self) -> Result<(), Error> {
        let w = self.data_stack.try_pop()?;
        let ptr = unsafe { w.ptr.cast::<Word>() };
        let val = unsafe { ptr.read() };
        self.data_stack.push(val)?;
        Ok(())
    }

    // TODO: Check alignment?
    pub fn var_store(&mut self) -> Result<(), Error> {
        let w_addr = self.data_stack.try_pop()?;
        let w_val = self.data_stack.try_pop()?;
        unsafe {
            w_addr.ptr.cast::<Word>().write(w_val);
        }
        Ok(())
    }

    pub fn zero_const(&mut self) -> Result<(), Error> {
        self.data_stack.push(Word::data(0))?;
        Ok(())
    }

    pub fn one_const(&mut self) -> Result<(), Error> {
        self.data_stack.push(Word::data(1))?;
        Ok(())
    }

    pub fn constant(&mut self) -> Result<(), Error> {
        let me = self.call_stack.try_peek()?;
        let de = me.eh.cast::<DictionaryEntry<T>>();
        let cfa = unsafe { DictionaryEntry::<T>::pfa(de) };
        let val = unsafe { cfa.as_ptr().read() };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn variable(&mut self) -> Result<(), Error> {
        let me = self.call_stack.try_peek()?;
        let de = me.eh.cast::<DictionaryEntry<T>>();
        let cfa = unsafe { DictionaryEntry::<T>::pfa(de) };
        let val = Word::ptr(cfa.as_ptr());
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn forget(&mut self) -> Result<(), Error> {
        // TODO: If anything we've defined in the dict has escaped into
        // the stack, variables, etc., we're definitely going to be in trouble.
        //
        // TODO: Check that we are in interpret and not compile mode?
        self.input.advance();
        let word = match self.input.cur_word() {
            None => return Err(Error::ForgetWithoutWordName),
            Some(s) => s,
        };
        let word_tmp = TmpFaStr::new_from(word);
        let defn = match self.find_in_dict(&word_tmp) {
            None => {
                if self.find_in_bis(&word_tmp).is_some() {
                    return Err(Error::CantForgetBuiltins);
                } else {
                    return Err(Error::ForgetNotInDict);
                }
            }
            Some(d) => d,
        };
        self.run_dict_tail = unsafe { defn.as_ref().link };
        let addr = defn.as_ptr();
        let contains = self.dict_alloc.contains(addr.cast());
        let ordered = (addr as usize) <= (self.dict_alloc.cur as usize);

        if !(contains && ordered) {
            return Err(Error::InternalError);
        }

        let len = (self.dict_alloc.cur as usize) - (addr as usize);
        unsafe {
            addr.write_bytes(0x00, len);
        }
        self.dict_alloc.cur = addr.cast();
        Ok(())
    }

    pub fn over(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_peek_back_n(1)?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn over_2(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_peek_back_n(2)?;
        let b = self.data_stack.try_peek_back_n(3)?;
        self.data_stack.push(b)?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn rot(&mut self) -> Result<(), Error> {
        let n1 = self.data_stack.try_pop()?;
        let n2 = self.data_stack.try_pop()?;
        let n3 = self.data_stack.try_pop()?;
        self.data_stack.push(n2)?;
        self.data_stack.push(n1)?;
        self.data_stack.push(n3)?;
        Ok(())
    }

    pub fn ds_drop(&mut self) -> Result<(), Error> {
        let _a = self.data_stack.try_pop()?;
        Ok(())
    }

    pub fn ds_drop_2(&mut self) -> Result<(), Error> {
        let _a = self.data_stack.try_pop()?;
        let _b = self.data_stack.try_pop()?;
        Ok(())
    }

    pub fn swap(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack.push(a)?;
        self.data_stack.push(b)?;
        Ok(())
    }

    pub fn swap_2(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let c = self.data_stack.try_pop()?;
        let d = self.data_stack.try_pop()?;
        self.data_stack.push(b)?;
        self.data_stack.push(a)?;
        self.data_stack.push(d)?;
        self.data_stack.push(c)?;
        Ok(())
    }

    pub fn space(&mut self) -> Result<(), Error> {
        self.output.push_bstr(b" ")?;
        Ok(())
    }

    pub fn spaces(&mut self) -> Result<(), Error> {
        let num = self.data_stack.try_pop()?;
        let num = unsafe { num.data };

        if num.is_negative() {
            return Err(Error::LoopCountIsNegative);
        }
        for _ in 0..num {
            self.space()?;
        }
        Ok(())
    }

    pub fn cr(&mut self) -> Result<(), Error> {
        self.output.push_bstr(b"\n")?;
        Ok(())
    }

    fn skip_literal(&mut self) -> Result<(), Error> {
        let parent = self.call_stack.try_peek_back_n_mut(1)?;
        parent.offset(1)?;
        Ok(())
    }

    pub fn invert(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let val = if a == Word::data(0) {
            Word::data(-1)
        } else {
            Word::data(0)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn and(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = Word::data(unsafe { a.data & b.data });
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn equal(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = if a == b {
            Word::data(-1)
        } else {
            Word::data(0)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn greater(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = if unsafe { b.data > a.data } { -1 } else { 0 };
        self.data_stack.push(Word::data(val))?;
        Ok(())
    }

    pub fn less(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = if unsafe { b.data < a.data } { -1 } else { 0 };
        self.data_stack.push(Word::data(val))?;
        Ok(())
    }

    pub fn zero_equal(&mut self) -> Result<(), Error> {
        self.data_stack.push(Word::data(0))?;
        self.equal()
    }

    pub fn zero_greater(&mut self) -> Result<(), Error> {
        self.data_stack.push(Word::data(0))?;
        self.greater()
    }

    pub fn zero_less(&mut self) -> Result<(), Error> {
        self.data_stack.push(Word::data(0))?;
        self.less()
    }

    pub fn float_div_mod(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        if unsafe { a.float == 0.0 } {
            return Err(Error::DivideByZero);
        }
        let rem = unsafe { Word::float(b.float % a.float) };
        self.data_stack.push(rem)?;
        let val = unsafe { Word::float(b.float / a.float) };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn div_mod(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        if unsafe { a.data == 0 } {
            return Err(Error::DivideByZero);
        }
        let rem = unsafe { Word::data(b.data % a.data) };
        self.data_stack.push(rem)?;
        let val = unsafe { Word::data(b.data / a.data) };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn float_div(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = unsafe {
            if a.float == 0.0 {
                return Err(Error::DivideByZero);
            }
            Word::float(b.float / a.float)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn div(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = unsafe {
            if a.data == 0 {
                return Err(Error::DivideByZero);
            }
            Word::data(b.data / a.data)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn float_modu(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = unsafe {
            if a.float == 0.0 {
                return Err(Error::DivideByZero);
            }
            Word::float(b.float % a.float)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn modu(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        let val = unsafe {
            if a.data == 0 {
                return Err(Error::DivideByZero);
            }
            Word::data(b.data % a.data)
        };
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn loop_i(&mut self) -> Result<(), Error> {
        let a = self.return_stack.try_peek()?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn loop_itick(&mut self) -> Result<(), Error> {
        let a = self.return_stack.try_peek_back_n(1)?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn loop_j(&mut self) -> Result<(), Error> {
        let a = self.return_stack.try_peek_back_n(2)?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn loop_leave(&mut self) -> Result<(), Error> {
        let _ = self.return_stack.try_pop()?;
        let a = self.return_stack.try_peek()?;
        self.return_stack
            .push(unsafe { Word::data(a.data.wrapping_sub(1)) })?;
        Ok(())
    }

    pub fn jump_doloop(&mut self) -> Result<(), Error> {
        let a = self.return_stack.try_pop()?;
        let b = self.return_stack.try_peek()?;
        let ctr = unsafe { Word::data(a.data + 1) };
        let do_jmp = ctr != b;
        if do_jmp {
            self.return_stack.push(ctr)?;
            self.jump()
        } else {
            self.return_stack.try_pop()?;
            self.skip_literal()
        }
    }

    pub fn emit(&mut self) -> Result<(), Error> {
        let val = self.data_stack.try_pop()?;
        let val = unsafe { val.data };
        self.output.push_bstr(&[val as u8])?;
        Ok(())
    }

    pub fn jump_if_zero(&mut self) -> Result<(), Error> {
        let do_jmp = unsafe {
            let val = self.data_stack.try_pop()?;
            val.data == 0
        };
        if do_jmp {
            self.jump()
        } else {
            self.skip_literal()
        }
    }

    pub fn jump(&mut self) -> Result<(), Error> {
        let parent = self.call_stack.try_peek_back_n_mut(1)?;
        let offset = parent.get_next_val()?;
        parent.offset(offset)?;
        Ok(())
    }

    pub fn dup(&mut self) -> Result<(), Error> {
        let val = self.data_stack.try_peek()?;
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn dup_2(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack.push(b)?;
        self.data_stack.push(a)?;
        self.data_stack.push(b)?;
        self.data_stack.push(a)?;
        Ok(())
    }

    pub fn return_to_data_stack(&mut self) -> Result<(), Error> {
        let val = self.return_stack.try_pop()?;
        self.data_stack.push(val)?;
        Ok(())
    }

    pub fn data_to_return_stack(&mut self) -> Result<(), Error> {
        let val = self.data_stack.try_pop()?;
        self.return_stack.push(val)?;
        Ok(())
    }

    pub fn data2_to_return2_stack(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.return_stack.push(b)?;
        self.return_stack.push(a)?;
        Ok(())
    }

    pub fn pop_print(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        write!(&mut self.output, "{} ", unsafe { a.data })
            .map_err(|_| OutputError::FormattingErr)?;
        Ok(())
    }

    pub fn unsigned_pop_print(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        write!(&mut self.output, "{} ", unsafe { a.data } as u32)
            .map_err(|_| OutputError::FormattingErr)?;
        Ok(())
    }

    pub fn float_pop_print(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        write!(&mut self.output, "{} ", unsafe { a.float })
            .map_err(|_| OutputError::FormattingErr)?;
        Ok(())
    }

    pub fn float_add(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float + b.float }))?;
        Ok(())
    }

    pub fn add(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.wrapping_add(b.data) }))?;
        Ok(())
    }

    pub fn float_mul(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float * b.float }))?;
        Ok(())
    }

    pub fn mul(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.wrapping_mul(b.data) }))?;
        Ok(())
    }

    #[cfg(feature = "use-std")]
    pub fn float_abs(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float.abs() }))?;
        Ok(())
    }

    #[cfg(not(feature = "use-std"))]
    pub fn float_abs(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        self.data_stack.push(Word::float(unsafe {
            if a.float.is_sign_negative() {
                a.float.neg()
            } else {
                a.float
            }
        }))?;
        Ok(())
    }

    pub fn abs(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.wrapping_abs() }))?;
        Ok(())
    }

    pub fn float_negate(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float.neg() }))?;
        Ok(())
    }

    pub fn negate(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.wrapping_neg() }))?;
        Ok(())
    }

    pub fn float_min(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float.min(b.float) }))?;
        Ok(())
    }

    pub fn min(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.min(b.data) }))?;
        Ok(())
    }

    pub fn float_max(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { a.float.max(b.float) }))?;
        Ok(())
    }

    pub fn max(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { a.data.max(b.data) }))?;
        Ok(())
    }

    pub fn float_minus(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::float(unsafe { b.float - a.float }))?;
        Ok(())
    }

    pub fn minus(&mut self) -> Result<(), Error> {
        let a = self.data_stack.try_pop()?;
        let b = self.data_stack.try_pop()?;
        self.data_stack
            .push(Word::data(unsafe { b.data.wrapping_sub(a.data) }))?;
        Ok(())
    }

    pub fn star_slash(&mut self) -> Result<(), Error> {
        let n3 = self.data_stack.try_pop()?;
        let n2 = self.data_stack.try_pop()?;
        let n1 = self.data_stack.try_pop()?;
        self.data_stack.push(Word::data(unsafe {
            (n1.data as i64)
                .wrapping_mul(n2.data as i64)
                .wrapping_div(n3.data as i64) as i32
        }))?;
        Ok(())
    }

    pub fn star_slash_mod(&mut self) -> Result<(), Error> {
        let n3 = self.data_stack.try_pop()?;
        let n2 = self.data_stack.try_pop()?;
        let n1 = self.data_stack.try_pop()?;
        unsafe {
            let top = (n1.data as i64).wrapping_mul(n2.data as i64);
            let div = n3.data as i64;
            let quo = top / div;
            let rem = top % div;
            self.data_stack.push(Word::data(rem as i32))?;
            self.data_stack.push(Word::data(quo as i32))?;
        }
        Ok(())
    }

    pub fn colon(&mut self) -> Result<(), Error> {
        self.input.advance();
        let name = self
            .input
            .cur_word()
            .ok_or(Error::ColonCompileMissingName)?;
        let old_mode = core::mem::replace(&mut self.mode, Mode::Compile);
        let name = self.dict_alloc.bump_str(name)?;

        // Allocate and initialize the dictionary entry
        //
        // TODO: Using `bump_write` here instead of just `bump` causes Miri to
        // get angry with a stacked borrows violation later when we attempt
        // to interpret a built word.
        let dict_base = self.dict_alloc.bump::<DictionaryEntry<T>>()?;

        let mut len = 0u16;

        // Begin compiling until we hit the end of the line or a semicolon.
        loop {
            let munched = self.munch_one(&mut len)?;
            if munched == 0 {
                match self.input.cur_word() {
                    Some(";") => {
                        unsafe {
                            dict_base.as_ptr().write(DictionaryEntry {
                                hdr: EntryHeader {
                                    // TODO: Should we look up `(interpret)` for consistency?
                                    // Use `find_word`?
                                    func: Self::interpret,
                                    name,
                                    kind: EntryKind::Dictionary,
                                    len,
                                },
                                // Don't link until we know we have a "good" entry!
                                link: self.run_dict_tail.take(),
                                parameter_field: [],
                            });
                        }
                        self.run_dict_tail = Some(dict_base);
                        self.mode = old_mode;
                        return Ok(());
                    }
                    Some(_) => {}
                    None => {
                        return Err(Error::ColonCompileMissingSemicolon);
                    }
                }
            }
        }
    }

    pub fn write_str_lit(&mut self) -> Result<(), Error> {
        let parent = self.call_stack.try_peek_back_n_mut(1)?;

        // The length in bytes is stored in the next word.
        let len = parent.get_next_val()?;
        let len_u16 = u16::try_from(len).replace_err(Error::LiteralStringTooLong)?;

        // Now we need to figure out how many words our inline string takes up
        let word_size = size_of::<Word>();
        let len_words = 1 + ((usize::from(len_u16) + (word_size - 1)) / word_size);
        let len_and_str = parent.get_next_n_words(len_words as u16)?;
        unsafe {
            // Skip the "len" word
            let start = len_and_str.as_ptr().add(1).cast::<u8>();
            // Then push the literal into the output buffer
            let u8_sli = core::slice::from_raw_parts(start, len_u16.into());
            self.output.push_bstr(u8_sli)?;
        }
        parent.offset(len_words as i32)?;
        Ok(())
    }

    /// `(literal)` is used mid-interpret to put the NEXT word of the parent's
    /// CFA array into the stack as a value.
    pub fn literal(&mut self) -> Result<(), Error> {
        let parent = self.call_stack.try_peek_back_n_mut(1)?;
        let literal = parent.get_next_word()?;
        parent.offset(1)?;
        self.data_stack.push(literal)?;
        Ok(())
    }

    /// Interpret is the run-time target of the `:` (colon) word.
    ///
    /// It is NOT considered a "builtin", as it DOES take the cfa, where
    /// other builtins do not.
    pub fn interpret(&mut self) -> Result<(), Error> {
        // Colon compiles into a list of words, where the first word
        // is a `u32` of the `len` number of words.
        //
        // NOTE: we DON'T use `Stack::try_peek_back_n_mut` because the callee
        // could pop off our item, which would lead to UB.
        let mut me = self.call_stack.try_peek()?;

        // For the remaining words, we do a while-let loop instead of
        // a for-loop, as some words (e.g. literals) require advancing
        // to the next word.
        while let Some(word) = me.get_word_at_cur_idx() {
            // We can safely assume that all items in the list are pointers,
            // EXCEPT for literals, but those are handled manually below.
            let ptr = unsafe { word.ptr.cast::<EntryHeader<T>>() };
            let nn = NonNull::new(ptr).ok_or(Error::NullPointerInCFA)?;
            let ehref = unsafe { nn.as_ref() };

            self.call_stack.overwrite_back_n(0, me)?;
            self.call_stack.push(CallContext {
                eh: nn,
                idx: 0,
                len: ehref.len,
            })?;
            let result = (ehref.func)(self);
            self.call_stack.try_pop()?;
            result?;
            me = self.call_stack.try_peek()?;

            me.offset(1)?;
            // TODO: If I want A4-style pausing here, I'd probably want to also
            // push dictionary locations to the stack (under the CFA), which
            // would allow for halting and resuming. Yield after loading "next",
            // right before executing the function itself. This would also allow
            // for cursed control flow
        }
        Ok(())
    }
}
