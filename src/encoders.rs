use std::io::Write;

use crate::{
    archive::{SevenZMethod, SevenZMethodConfiguration},
    lzma::CountingWriter,
    lzma::{LZMA2Options, LZMA2Writer},
    method_options::MethodOptions,
    Error,
};

pub enum Encoder<W: Write> {
    LZMA2(LZMA2Writer<W>),
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Encoder::LZMA2(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Encoder::LZMA2(w) => w.flush(),
        }
    }
}

pub fn add_encoder<W: Write>(
    input: CountingWriter<W>,
    method_config: &SevenZMethodConfiguration,
) -> Result<Encoder<W>, Error> {
    let method = method_config.method;

    match method.id() {
        SevenZMethod::ID_LZMA2 => {
            let mut def_opts = LZMA2Options::default();
            let options = match method_config.options.as_ref() {
                Some(MethodOptions::LZMA2(opts)) => opts,
                Some(MethodOptions::Num(n)) => {
                    def_opts.dict_size = *n;
                    &def_opts
                }
                _ => {
                    def_opts.dict_size = LZMA2Options::DICT_SIZE_DEFAULT;
                    &def_opts
                }
            };
            let lz = LZMA2Writer::new(input, options);
            Ok(Encoder::LZMA2(lz))
        }
        _ => {
            return Err(Error::UnsupportedCompressionMethod(
                method.name().to_string(),
            ));
        }
    }
}

pub(crate) fn get_options_as_properties<'a>(
    method: SevenZMethod,
    options: Option<&MethodOptions>,
    out: &'a mut [u8],
) -> &'a [u8] {
    match method.id() {
        SevenZMethod::ID_LZMA2 => {
            let dict_size = options
                .map(|o| o.get_lzma2_dict_size())
                .unwrap_or(LZMA2Options::DICT_SIZE_DEFAULT);
            let lead = dict_size.leading_zeros();
            let second_bit = (dict_size >> (30u32.wrapping_sub(lead))).wrapping_sub(2);
            let prop = (19u32.wrapping_sub(lead) * 2 + second_bit) as u8;
            out[0] = prop;
            &out[0..1]
        }
        _ => {
            if let Some(MethodOptions::Raw(props)) = options {
                out[..props.len()].copy_from_slice(props);
                &out[..props.len()]
            } else {
                &[]
            }
        }
    }
}
