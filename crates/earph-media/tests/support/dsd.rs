// ABOUTME: Builds generated DSF and DFF containers for real reader tests.
// ABOUTME: Shares valid and mutable container constructors across software pipelines.
pub fn chunk(id: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend((bytes.len() as u64).to_be_bytes());
    out.extend(bytes);
    if !bytes.len().is_multiple_of(2) {
        out.push(0);
    }
    out
}
pub fn properties(rate: u32) -> Vec<u8> {
    let mut p = b"SND ".to_vec();
    p.extend(chunk(b"FS  ", &rate.to_be_bytes()));
    p.extend(chunk(
        b"CHNL",
        &[0, 2, b'S', b'L', b'F', b'T', b'S', b'R', b'G', b'T'],
    ));
    p.extend(chunk(b"CMPR", b"DSD \x03raw"));
    p
}
pub fn form(children: &[u8]) -> Vec<u8> {
    let mut payload = b"DSD ".to_vec();
    payload.extend(children);
    chunk(b"FRM8", &payload)
}
pub fn dff(canonical: &[u8], rate: u32) -> Vec<u8> {
    let mut children = chunk(b"FVER", &[1, 5, 0, 0]);
    children.extend(chunk(b"PROP", &properties(rate)));
    children.extend(chunk(b"JUNK", b"odd"));
    children.extend(chunk(b"DSD ", canonical));
    children.extend(chunk(b"TAIL", b"bounded"));
    form(&children)
}
pub fn dsf(canonical: &[u8], order: u32, rate: u32, tag: &[u8]) -> Vec<u8> {
    let per_channel = canonical.len() / 2;
    let payload = per_channel.div_ceil(4096) * 8192;
    let data_end = 92 + payload;
    let mut out = Vec::new();
    out.extend(b"DSD ");
    out.extend(28u64.to_le_bytes());
    out.extend(((data_end + tag.len()) as u64).to_le_bytes());
    out.extend((if tag.is_empty() { 0 } else { data_end as u64 }).to_le_bytes());
    out.extend(b"fmt ");
    out.extend(52u64.to_le_bytes());
    for value in [1, 0, 2, 2, rate, order] {
        out.extend(value.to_le_bytes());
    }
    out.extend(((per_channel * 8) as u64).to_le_bytes());
    out.extend(4096u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(b"data");
    out.extend((12 + payload as u64).to_le_bytes());
    for group in 0..per_channel.div_ceil(4096) {
        for channel in 0..2 {
            for byte in 0..4096 {
                let i = group * 4096 + byte;
                let value = if i < per_channel {
                    canonical[2 * i + channel]
                } else {
                    0
                };
                out.push(if order == 1 {
                    value.reverse_bits()
                } else {
                    value
                });
            }
        }
    }
    out.extend(tag);
    out
}
