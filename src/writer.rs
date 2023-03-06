use crate::{archive::*, encoders, lzma::*, reader::CRC32, Error, SevenZArchiveEntry};
use bit_set::BitSet;
use byteorder::*;
use std::{
    cell::Cell,
    collections::HashMap,
    fs::File,
    io::{Read, Seek, Write},
    path::Path,
    rc::Rc,
    sync::Arc,
};

macro_rules! write_times {
    //write_i64
    ($fn_name:tt, $nid:expr, $has_time:tt, $time:tt) => {
        write_times!($fn_name, $nid, $has_time, $time, write_u64);
    };
    ($fn_name:tt, $nid:expr, $has_time:tt, $time:tt, $write_fn:tt) => {
        fn $fn_name<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
            let mut num = 0;
            for entry in self.files.iter() {
                if entry.$has_time {
                    num += 1;
                }
            }
            if num > 0 {
                header.write_u8($nid)?;
                let mut temp: Vec<u8> = Vec::with_capacity(128);
                let mut out = &mut temp;
                if num != self.files.len() {
                    out.write_u8(0)?;
                    let mut times = BitSet::with_capacity(self.files.len());
                    for i in 0..self.files.len() {
                        if self.files[i].$has_time {
                            times.insert(i);
                        }
                    }
                    write_bit_set(&mut out, &times)?;
                } else {
                    out.write_u8(1)?;
                }
                out.write_u8(0)?;
                for file in self.files.iter() {
                    if file.$has_time {
                        out.$write_fn::<LittleEndian>((file.$time).into())?;
                    }
                }
                out.flush()?;
                write_u64(header, temp.len() as u64)?;
                header.write_all(&temp)?;
            }
            Ok(())
        }
    };
}

type Result<T> = std::result::Result<T, crate::Error>;

/// Writes a 7z file
pub struct SevenZWriter<W: Write> {
    output: W,
    files: Vec<SevenZArchiveEntry>,
    content_methods: Arc<Vec<SevenZMethodConfiguration>>,
    additional_sizes: HashMap<String, Vec<usize>>,
    num_non_empty_streams: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl SevenZWriter<File> {
    /// Creates a file to write a 7z archive to
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let file = std::fs::File::create(path.as_ref())
            .map_err(|e| crate::Error::file_open(e, path.as_ref().to_string_lossy().to_string()))?;
        Self::new(file)
    }
}
impl<W: Write + Seek> SevenZWriter<W> {
    /// Prepares writer to write a 7z archive to
    pub fn new(mut writer: W) -> Result<Self> {
        writer
            .seek(std::io::SeekFrom::Start(
                crate::archive::SIGNATURE_HEADER_SIZE,
            ))
            .map_err(Error::io)?;

        Ok(Self {
            output: writer,
            files: Default::default(),
            content_methods: Arc::new(vec![SevenZMethodConfiguration::new(SevenZMethod::LZMA2)]),
            additional_sizes: Default::default(),
            num_non_empty_streams: 0,
        })
    }

    /// Sets the default compression methods to use for entry contents.
    /// The default is LZMA2.
    /// And currently only support LZMA2
    ///
    pub fn set_content_methods(&mut self, content_methods: Arc<Vec<SevenZMethodConfiguration>>) {
        self.content_methods = content_methods;
    }

    /// Create an archive entry using the file in `path` and entry_name provided.
    pub fn create_archive_entry(path: impl AsRef<Path>, entry_name: String) -> SevenZArchiveEntry {
        let path = path.as_ref();

        let mut entry = SevenZArchiveEntry {
            name: entry_name,
            has_stream: path.is_file(),
            is_directory: path.is_dir(),
            ..Default::default()
        };

        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                entry.last_modified_date = modified.into();
                entry.has_last_modified_date = entry.last_modified_date.raw() > 0;
            }
        }
        entry
    }

    /// Adds an archive `entry` with data from `reader`
    pub fn push_archive_entry<R: Read>(
        &mut self,
        mut entry: SevenZArchiveEntry,
        reader: Option<R>,
    ) -> Result<&SevenZArchiveEntry> {
        if !entry.is_directory {
            if let Some(mut r) = reader {
                if entry.content_methods.is_empty() {
                    entry.content_methods = self.content_methods.clone();
                }
                let mut compressed_len = 0;
                let mut compressed = CompressWrapWriter::new(&mut self.output, &mut compressed_len);

                let mut more_sizes: Vec<Rc<Cell<usize>>> =
                    Vec::with_capacity(entry.content_methods.len() - 1);

                let (crc, size) = {
                    let mut w = Self::create_writer(&mut entry, &mut compressed, &mut more_sizes)?;
                    let mut write_len = 0;
                    let mut w = CompressWrapWriter::new(&mut w, &mut write_len);
                    let mut buf = [0u8; 4096];
                    loop {
                        match r.read(&mut buf) {
                            Ok(n) => {
                                if n == 0 {
                                    break;
                                }
                                w.write_all(&buf[..n]).map_err(|e| {
                                    Error::io_msg(e, format!("Encode entry:{}", entry.name()))
                                })?;
                            }
                            Err(e) => {
                                return Err(Error::io_msg(
                                    e,
                                    format!("Encode entry:{}", entry.name()),
                                ));
                            }
                        }
                    }
                    w.flush()
                        .map_err(|e| Error::io_msg(e, format!("Encode entry:{}", entry.name())))?;
                    w.write(&[])
                        .map_err(|e| Error::io_msg(e, format!("Encode entry:{}", entry.name())))?;

                    (w.crc_value(), write_len)
                };
                let compressed_crc = compressed.crc_value() as u64;
                entry.has_stream = true;
                entry.size = size as u64;
                entry.crc = crc as u64;
                entry.has_crc = true;
                entry.compressed_crc = compressed_crc;
                entry.compressed_size = compressed_len as u64;
                if !more_sizes.is_empty() {
                    self.additional_sizes.insert(
                        entry.name.clone(),
                        more_sizes.iter().map(|c| c.get()).collect(),
                    );
                }
                self.num_non_empty_streams += 1;
                self.files.push(entry);
                return Ok(self.files.last().unwrap());
            }
        }
        entry.has_stream = false;
        entry.size = 0;
        entry.compressed_size = 0;
        entry.has_crc = false;
        self.files.push(entry);
        Ok(self.files.last().unwrap())
    }

    pub fn push_encoded_entry<R: Read>(
        &mut self,
        entry: SevenZArchiveEntry,
        additional_sizes: Vec<usize>,
        mut reader: R,
    ) -> Result<()> {
        if entry.has_stream {
            let mut buf = [0; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(size) => {
                        if size > 0 {
                            self.output
                                .write_all(&buf[..size])
                                .map_err(|e| Error::io(e))?;
                        } else {
                            break;
                        }
                    }
                    Err(e) => return Err(Error::io(e)),
                }
            }
            // let _ = std::io::copy(&mut reader, &mut self.output).map_err(|e| Error::io(e))?;
            self.num_non_empty_streams += 1;
            self.additional_sizes
                .insert(entry.name.clone(), additional_sizes);
        }
        self.files.push(entry);
        Ok(())
    }

    fn create_writer<'a, O: Write + 'a>(
        entry: &mut SevenZArchiveEntry,
        out: O,
        more_sized: &mut Vec<Rc<Cell<usize>>>,
    ) -> Result<Box<dyn Write + 'a>> {
        let methods = &entry.content_methods;
        let mut encoder: Box<dyn Write> = Box::new(out);
        let mut first = true;
        for mc in methods.iter() {
            if !first {
                let counting = CountingWriter::new(encoder);
                more_sized.push(counting.counting());
                encoder = Box::new(encoders::add_encoder(counting, mc)?);
            } else {
                let counting = CountingWriter::new(encoder);
                encoder = Box::new(encoders::add_encoder(counting, mc)?);
            }
            first = false;
        }
        Ok(encoder)
    }

    /// Finishes the compression.
    pub fn finish(mut self) -> std::io::Result<()> {
        let header_pos = self.output.stream_position()?;
        let mut header: Vec<u8> = Vec::with_capacity(64 * 1024);
        self.write_header(&mut header)?;
        self.output.write_all(&header)?;
        let crc32 = CRC32.checksum(&header);
        let mut hh = [0u8; SIGNATURE_HEADER_SIZE as usize];
        {
            let mut hhw = hh.as_mut_slice();
            //sig
            hhw.write_all(&SEVEN_Z_SIGNATURE)?;
            //version
            hhw.write_u8(0)?;
            hhw.write_u8(2)?;
            //placeholder for crc: index = 8
            hhw.write_u32::<LittleEndian>(0)?;

            // start header
            hhw.write_u64::<LittleEndian>(header_pos - SIGNATURE_HEADER_SIZE)?;
            hhw.write_u64::<LittleEndian>(0xffffffff & header.len() as u64)?;
            hhw.write_u32::<LittleEndian>(crc32)?;
        }
        let crc32 = CRC32.checksum(&hh[12..]);
        hh[8..12].copy_from_slice(&crc32.to_le_bytes());

        self.output.seek(std::io::SeekFrom::Start(0))?;
        self.output.write(&hh)?;
        Ok(())
    }

    fn write_header<H: Write>(&mut self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_HEADER)?;
        header.write_u8(K_MAIN_STREAMS_INFO)?;
        self.write_streams_info(header)?;
        self.write_files_info(header)?;
        header.write_u8(K_END)?;
        Ok(())
    }

    fn write_streams_info<H: Write>(&mut self, header: &mut H) -> std::io::Result<()> {
        if self.num_non_empty_streams > 0 {
            self.write_pack_info(header)?;
            self.write_unpack_info(header)?;
        }
        self.write_sub_streams_info(header)?;
        header.write_u8(K_END)?;
        Ok(())
    }

    fn write_pack_info<H: Write>(&mut self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_PACK_INFO)?;
        write_u64(header, 0)?;
        write_u64(header, self.num_non_empty_streams as u64)?;
        header.write_u8(K_SIZE)?;
        for entry in self.files.iter() {
            if entry.has_stream {
                write_u64(header, entry.compressed_size)?;
            }
        }
        header.write_u8(K_CRC)?;
        let all_crc_defined = self.files.iter().all(|f| f.compressed_crc != 0);
        if all_crc_defined {
            header.write_u8(1)?; // all defined
            for entry in self.files.iter() {
                if entry.has_stream {
                    header.write_u32::<LittleEndian>(entry.compressed_crc as u32)?;
                }
            }
        } else {
            header.write_u8(0)?; // all defined
            let mut crc_define_bits = BitSet::with_capacity(self.num_non_empty_streams);

            let mut i = 0;
            for entry in self.files.iter().filter(|f| f.has_stream) {
                if entry.compressed_crc != 0 {
                    crc_define_bits.insert(i);
                }
                i += 1;
            }
            let mut temp = Vec::with_capacity(self.num_non_empty_streams);
            write_bit_set(&mut temp, &crc_define_bits)?;
            header.write_all(&temp)?;
            for entry in self.files.iter() {
                if entry.has_stream && entry.compressed_crc != 0 {
                    header.write_u32::<LittleEndian>(entry.compressed_crc as u32)?;
                }
            }
        }

        header.write_u8(K_END)?;
        Ok(())
    }
    fn write_unpack_info<H: Write>(&mut self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_UNPACK_INFO)?;
        header.write_u8(K_FOLDER)?;
        write_u64(header, self.num_non_empty_streams as u64)?;
        header.write_u8(0)?;
        let mut cache = Vec::with_capacity(32);
        for entry in self.files.iter() {
            if entry.has_stream {
                self.write_folder(header, entry, &mut cache)?;
            }
        }
        header.write_u8(K_CODERS_UNPACK_SIZE)?;
        for entry in self.files.iter() {
            if entry.has_stream {
                if let Some(sized) = self.additional_sizes.get(entry.name()) {
                    for s in sized {
                        write_u64(header, *s as u64)?;
                    }
                }
                write_u64(header, entry.size)?;
            }
        }
        header.write_u8(K_CRC)?;
        header.write_u8(1)?; //all defined
        for entry in self.files.iter() {
            if entry.has_stream {
                header.write_u32::<LittleEndian>(entry.crc as u32)?;
            }
        }
        header.write_u8(K_END)?;
        Ok(())
    }

    fn write_folder<H: Write>(
        &self,
        header: &mut H,
        entry: &SevenZArchiveEntry,
        cache: &mut Vec<u8>,
    ) -> std::io::Result<()> {
        cache.clear();
        let mut num_coders = 0;
        for mc in entry.content_methods.iter() {
            num_coders += 1;
            self.write_single_codec(mc, cache)?;
        }
        write_u64(header, num_coders as u64)?;
        header.write(cache)?;
        for i in 0..num_coders - 1 {
            write_u64(header, i as u64 + 1)?;
            write_u64(header, i as u64)?;
        }
        Ok(())
    }

    fn write_single_codec<H: Write>(
        &self,
        mc: &SevenZMethodConfiguration,
        cache: &mut H,
    ) -> std::io::Result<()> {
        let id = mc.method.id();
        let mut temp = [0u8; 6];
        let props = encoders::get_options_as_properties(mc.method, mc.options.as_ref(), &mut temp);
        let mut codec_flags = id.len() as u8;
        if props.len() > 0 {
            codec_flags |= 0x20;
        }
        cache.write_u8(codec_flags)?;
        cache.write(id)?;
        if props.len() > 0 {
            cache.write_u8(props.len() as u8)?;
            cache.write(props)?;
        }
        Ok(())
    }
    fn write_sub_streams_info<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_SUB_STREAMS_INFO)?;
        header.write_u8(K_END)?;
        Ok(())
    }
    fn write_files_info<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_FILES_INFO)?;
        write_u64(header, self.files.len() as u64)?;
        self.write_file_empty_streams(header)?;
        self.write_file_empty_files(header)?;
        self.write_file_anti_items(header)?;
        self.write_file_names(header)?;
        self.write_file_ctimes(header)?;
        self.write_file_atimes(header)?;
        self.write_file_mtimes(header)?;
        self.write_file_windows_attrs(header)?;
        header.write_u8(K_END)?;
        Ok(())
    }

    fn write_file_empty_streams<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        let mut has_empty = false;
        for entry in self.files.iter() {
            if !entry.has_stream {
                has_empty = true;
                break;
            }
        }
        if has_empty {
            header.write_u8(K_EMPTY_STREAM)?;
            let mut bitset = BitSet::with_capacity(self.files.len());
            let mut i = 0;
            for entry in self.files.iter() {
                if !entry.has_stream {
                    bitset.insert(i);
                }
                i += 1;
            }
            let mut temp: Vec<u8> = Vec::with_capacity(bitset.len() / 8 + 1);
            write_bit_set(&mut temp, &bitset)?;
            write_u64(header, temp.len() as u64)?;
            header.write(temp.as_slice())?;
        }
        Ok(())
    }
    fn write_file_empty_files<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        let mut has_empty = false;
        let mut empty_stream_counter = 0;
        let mut bitset = BitSet::new();
        for entry in self.files.iter() {
            if !entry.has_stream {
                let is_dir = entry.is_directory();
                has_empty = has_empty | !is_dir;
                if !is_dir {
                    bitset.insert(empty_stream_counter);
                }
                empty_stream_counter += 1;
            }
        }
        if has_empty {
            header.write_u8(K_EMPTY_FILE)?;

            let mut temp: Vec<u8> = Vec::with_capacity(bitset.len() / 8 + 1);
            write_bit_set(&mut temp, &bitset)?;
            write_u64(header, temp.len() as u64)?;
            header.write(temp.as_slice())?;
        }
        Ok(())
    }

    fn write_file_anti_items<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        let mut has_anti = false;
        let mut counter = 0;
        let mut bitset = BitSet::new();
        for entry in self.files.iter() {
            if !entry.has_stream {
                let is_anti = entry.is_anti_item();
                has_anti = has_anti | !is_anti;
                if !is_anti {
                    bitset.insert(counter);
                }
                counter += 1;
            }
        }
        if has_anti {
            header.write_u8(K_ANTI)?;

            let mut temp: Vec<u8> = Vec::with_capacity(bitset.len() / 8 + 1);
            write_bit_set(&mut temp, &bitset)?;
            write_u64(header, temp.len() as u64)?;
            header.write(temp.as_slice())?;
        }
        Ok(())
    }
    fn write_file_names<H: Write>(&self, header: &mut H) -> std::io::Result<()> {
        header.write_u8(K_NAME)?;
        let mut temp: Vec<u8> = Vec::with_capacity(128);
        let out = &mut temp;
        out.write_u8(0)?;
        for file in self.files.iter() {
            for c in file.name().encode_utf16() {
                let buf = c.to_le_bytes();
                out.write_all(&buf)?;
            }
            out.write_all(&[0u8; 2])?;
        }
        write_u64(header, temp.len() as u64)?;
        header.write_all(temp.as_slice())?;
        Ok(())
    }

    write_times!(
        write_file_ctimes,
        K_C_TIME,
        has_creation_date,
        creation_date
    );
    write_times!(write_file_atimes, K_A_TIME, has_access_date, access_date);
    write_times!(
        write_file_mtimes,
        K_M_TIME,
        has_last_modified_date,
        last_modified_date
    );
    write_times!(
        write_file_windows_attrs,
        K_WIN_ATTRIBUTES,
        has_windows_attributes,
        windows_attributes,
        write_u32
    );
}

fn write_u64<W: Write>(header: &mut W, mut value: u64) -> std::io::Result<()> {
    let mut first = 0;
    let mut mask = 0x80;
    let mut i = 0;
    while i < 8 {
        if value < (1u64 << (7 * (i + 1))) {
            first |= value >> (8 * i);
            break;
        }
        first |= mask;
        mask = mask >> 1;
        i += 1;
    }
    header.write_u8((first & 0xff) as u8)?;
    while i > 0 {
        header.write_u8((value & 0xff) as u8)?;
        value = value >> 8;
        i -= 1;
    }
    Ok(())
}

fn write_bit_set<W: Write>(mut write: W, bs: &BitSet) -> std::io::Result<()> {
    let mut cache = 0;
    let mut shift = 7;
    for i in 0..bs.get_ref().len() {
        let set = if bs.contains(i) { 1 } else { 0 };
        cache |= set << shift;
        shift -= 1;
        if shift < 0 {
            write.write_u8(cache)?;
            shift = 7;
            cache = 0;
        }
    }
    if shift != 7 {
        write.write_u8(cache)?;
    }
    Ok(())
}

struct CompressWrapWriter<'a, W> {
    writer: W,
    crc: crc::Digest<'static, u32>,
    cache: Vec<u8>,
    bytes_written: &'a mut usize,
}
impl<'a, W: Write> CompressWrapWriter<'a, W> {
    pub fn new(writer: W, bytes_written: &'a mut usize) -> Self {
        Self {
            writer,
            crc: crate::reader::CRC32.digest(),
            cache: Vec::with_capacity(8192),
            bytes_written,
        }
    }

    pub fn crc_value(&mut self) -> u32 {
        let crc = std::mem::replace(&mut self.crc, crate::reader::CRC32.digest());
        crc.finalize()
    }
}

impl<'a, W: Write> Write for CompressWrapWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.cache.resize(buf.len(), Default::default());
        let len = self.writer.write(buf)?;
        self.crc.update(&buf[..len]);
        *self.bytes_written = *self.bytes_written + len;
        Ok(len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
