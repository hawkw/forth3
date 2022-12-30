use core::hash::Hasher as _;
use core::{marker::PhantomData, ops::Deref};
use hash32::{FnvHasher, Hasher};

pub struct TmpFaStr<'a> {
    stir: PhantomData<&'a str>,
    fastr: FaStr,
}

impl<'a> Deref for TmpFaStr<'a> {
    type Target = FaStr;

    fn deref(&self) -> &Self::Target {
        &self.fastr
    }
}

impl<'a> TmpFaStr<'a> {
    pub fn new_from(stir: &'a str) -> Self {
        let fastr = unsafe { FaStr::new(stir.as_ptr(), stir.len()) };
        Self {
            fastr,
            stir: PhantomData,
        }
    }
}

pub struct FaStr {
    ptr: *const u8,
    len_hash: LenHash,
}

impl FaStr {
    pub unsafe fn new(addr: *const u8, len: usize) -> Self {
        let u8_sli = core::slice::from_raw_parts(addr, len);
        let len_hash = LenHash::from_bstr(u8_sli);
        Self {
            ptr: addr,
            len_hash,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        let len = self.len_hash.len();
        unsafe { core::slice::from_raw_parts(self.ptr, len) }
    }

    pub fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }
}

impl PartialEq for FaStr {
    fn eq(&self, other: &Self) -> bool {
        if self.len_hash.eq_ignore_bits(&other.len_hash) {
            self.as_bytes().eq(other.as_bytes())
        } else {
            false
        }
    }
}

pub struct LenHash {
    // 29..32: 3-bit bitfield
    // 24..29: 5-bit len (0..31)
    // 00..24: 24-bit FnvHash
    inner: u32,
}

impl LenHash {
    const HASH_MASK: u32 = 0x00FF_FFFF;
    const BITS_MASK: u32 = 0xE000_0000;
    const LEN_MASK: u32 = 0x1F00_0000;

    /// Creates a new LenHash, considering UP TO 31 ascii characters.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self::from_bstr(s.as_bytes())
    }

    pub fn from_bstr(s: &[u8]) -> Self {
        let mut hasher = FnvHasher::default();
        let len = s.len().min(31);

        // TODO: I COULD hash more than 31 chars, which might give us some
        // chance of having longer strings, but we couldn't detect collisions
        // for strings longer than that. Maybe, but seems niche.
        hasher.write(&s[..len]);
        let hash = hasher.finish32();
        let inner = ((len as u32) << 24) | (hash & Self::HASH_MASK);
        Self { inner }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        let len_u32 = (self.inner & Self::LEN_MASK) >> 24;
        len_u32 as usize
    }

    pub fn bits(&self) -> u8 {
        let bits_u32 = (self.inner & Self::BITS_MASK) >> 29;
        bits_u32 as u8
    }

    pub fn eq_ignore_bits(&self, other: &Self) -> bool {
        (self.inner & !Self::BITS_MASK) == (other.inner & !Self::BITS_MASK)
    }
}