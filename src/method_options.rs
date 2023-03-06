use std::fmt::Debug;

#[derive(Debug, Clone)]
pub enum MethodOptions {
    Num(u32),
    #[cfg(feature = "compress")]
    LZMA2(crate::lzma::LZMA2Options),
    Raw(Vec<u8>),
}

impl From<u32> for MethodOptions {
    fn from(n: u32) -> Self {
        Self::Num(n)
    }
}

#[cfg(feature = "compress")]
impl From<crate::lzma::LZMA2Options> for MethodOptions {
    fn from(o: crate::lzma::LZMA2Options) -> Self {
        Self::LZMA2(o)
    }
}

impl MethodOptions {
    pub fn get_lzma2_dict_size(&self) -> u32 {
        match self {
            MethodOptions::Num(n) => *n,
            #[cfg(feature = "compress")]
            MethodOptions::LZMA2(o) => o.dict_size,
            MethodOptions::Raw(props) => {
                crate::decoders::get_lzma2_dic_size(props).unwrap_or_default()
            }
        }
    }
}
