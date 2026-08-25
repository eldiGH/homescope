use probe_rs::MemoryInterface;

pub trait MemoryExt: MemoryInterface<probe_rs::Error> {
    fn find_mismatch(
        &mut self,
        mut address: u64,
        words: &[u32],
    ) -> Result<Option<Mismatch>, probe_rs::Error> {
        const CHUNK_SIZE: usize = 128;
        let mut buffer = [0u32; CHUNK_SIZE];

        for chunk in words.chunks(CHUNK_SIZE) {
            let buf_slice = &mut buffer[..chunk.len()];

            self.read_32(address, buf_slice)?;

            for (i, (&actual, &expected)) in buf_slice.iter().zip(chunk.iter()).enumerate() {
                let mismatch_address = address + (i as u64 * 4);
                if actual != expected {
                    return Ok(Some(Mismatch {
                        address: mismatch_address,
                        actual,
                        expected,
                    }));
                }
            }

            address += (chunk.len() as u64) * 4;
        }

        Ok(None)
    }

    fn read_words<const N: usize>(&mut self, address: u64) -> Result<[u32; N], probe_rs::Error> {
        let mut buffer = [0u32; N];

        self.read_32(address, &mut buffer)?;

        Ok(buffer)
    }
}

impl<T: MemoryInterface<probe_rs::Error> + ?Sized> MemoryExt for T {}

#[derive(Debug)]
pub struct Mismatch {
    pub address: u64,
    pub expected: u32,
    pub actual: u32,
}
