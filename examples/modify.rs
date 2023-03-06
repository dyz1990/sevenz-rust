use sevenz_rust::SevenZArchiveEntry;

fn main() {
    let sr = sevenz_rust::SevenZReader::open("examples/data/diffuser-types.7z", "".into()).expect("ok");

    let mut sw = sevenz_rust::SevenZWriter::create("examples/data/diffuser-types-modified.7z").unwrap();

    sr.decode_each_entries(|mut decoder| {
        if decoder.entry().name() == "tests/readme.md" {
            let mut entry = SevenZArchiveEntry::default();
            {
                let old = decoder.entry();
                entry.name = old.name.to_string();
                entry.copy_date_from(old);
            }
            let mut content = String::new();
            decoder
                .decoded_reader()?
                .read_to_string(&mut content)
                .map_err(|e| sevenz_rust::Error::io(e))?;
            content.push_str("\nmodified");

            sw.push_archive_entry(entry, Some(content.as_bytes()))?;
        } else {
            let mut unpack_sizes: Vec<_> =
                decoder.unpack_sizes().iter().map(|s| *s as usize).collect();

            unpack_sizes.pop();
            sw.push_encoded_entry(
                decoder.entry().clone(),
                unpack_sizes,
                decoder.undecoded_reader()?,
            )?;
        }
        Ok(true)
    })
    .expect("OK");
    sw.finish().unwrap();
}
