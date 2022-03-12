use crate::error;
use std::io;

pub struct Reader<'a, D, Buffered = D>(ReaderImpl<'a, D, Buffered>);

#[derive(Default)]
pub struct Decompressor {
    #[cfg(feature = "read-deflate")]
    deflate: Option<Box<(miniz_oxide::inflate::core::DecompressorOxide, InflateBuffer)>>,
}

pub struct ReaderBuilder<'a, D> {
    disk: D,
    imp: ReaderImpl<'a, (), ()>,
    start: u64,
}

enum ReaderImpl<'a, D, Buffered> {
    Never {
        never: core::convert::Infallible,
        b: Buffered,
        stored: &'a mut (),
    },
    Stored {
        disk: D,
        remaining: u64,
    },
    #[cfg(feature = "read-deflate")]
    Deflate {
        disk: Buffered,
        remaining: u64,
        decompressor: &'a mut (miniz_oxide::inflate::core::DecompressorOxide, InflateBuffer),
        out_pos: u16,
        read_cursor: u16,
    },
}

impl<'a, D> ReaderBuilder<'a, D> {
    pub(super) fn new(
        disk: D,
        header: super::FileHeader,
        decompressor: &'a mut Decompressor,
    ) -> Result<Self, error::MethodNotSupported> {
        Ok(Self {
            disk,
            start: header.start,
            imp: match header.method {
                zip_format::CompressionMethod::STORED => ReaderImpl::Stored {
                    disk: (),
                    remaining: header.len,
                },
                #[cfg(feature = "read-deflate")]
                zip_format::CompressionMethod::DEFLATE => ReaderImpl::Deflate {
                    disk: (),
                    remaining: header.len,
                    decompressor: {
                        let deflate = decompressor.deflate.get_or_insert_with(Default::default);
                        deflate.0.init();
                        deflate
                    },
                    out_pos: 0,
                    read_cursor: 0,
                },
                _ => return Err(error::MethodNotSupported(())),
            },
        })
    }
}

impl<'a, D: io::Seek + io::Read> ReaderBuilder<'a, D> {
    pub fn seek_to_data<Buffered>(
        mut self,
        into_buffered: impl FnOnce(D) -> Buffered,
    ) -> io::Result<Reader<'a, D, Buffered>> {
        // TODO: avoid seeking if we can, since this will often be done in a loop
        self.disk.seek(std::io::SeekFrom::Start(self.start))?;
        let mut buf = [0; std::mem::size_of::<zip_format::Header>() + 4];
        self.disk.read_exact(&mut buf)?;
        let header = zip_format::Header::as_prefix(&buf).ok_or(error::NotAnArchive(()))?;
        self.disk.seek(std::io::SeekFrom::Current(
            header.name_len.get() as i64 + header.metadata_len.get() as i64,
        ))?;
        Ok(Reader(match self.imp {
            ReaderImpl::Never { never, .. } => match never {},
            ReaderImpl::Stored {
                remaining,
                disk: (),
            } => ReaderImpl::Stored {
                remaining,
                disk: self.disk,
            },
            #[cfg(feature = "read-deflate")]
            ReaderImpl::Deflate {
                remaining,
                decompressor,
                out_pos,
                read_cursor,
                disk: (),
            } => ReaderImpl::Deflate {
                disk: into_buffered(self.disk),
                remaining,
                decompressor,
                out_pos,
                read_cursor,
            },
        }))
    }
}

impl<D: io::Read, Buffered: io::BufRead> io::Read for Reader<'_, D, Buffered> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            ReaderImpl::Never { never, .. } => match *never {},
            ReaderImpl::Stored { disk, remaining } => {
                let n = disk.take(*remaining).read(buf)?;
                *remaining -= n as u64;
                Ok(n)
            }
            #[cfg(feature = "read-deflate")]
            ReaderImpl::Deflate {
                disk,
                decompressor,
                out_pos,
                read_cursor,
                ..
            } => {
                // TODO: track `remaining`
                // TODO: Check the CRC of the decompressed data
                let (decompressor, InflateBuffer(backbuf)) = decompressor;
                let mut out = *out_pos as usize;
                let mut cursor = *read_cursor as usize;
                if read_cursor == out_pos {
                    let data = disk.fill_buf()?;
                    if out == backbuf.len() {
                        out = 0;
                    }
                    let (_status, consumed, written) =
                        miniz_oxide::inflate::core::decompress(decompressor, data, backbuf, out, 0);
                    out += written;
                    disk.consume(consumed);
                }
                if cursor == backbuf.len() {
                    cursor = 0;
                }
                let len = out.checked_sub(cursor).unwrap().min(buf.len());
                buf[..len].copy_from_slice(&backbuf[cursor..][..len]);
                *read_cursor = (cursor + len) as u16;
                *out_pos = out as u16;
                Ok(len)
            }
        }
    }
}

struct InflateBuffer([u8; 32 * 1024]);
impl Default for InflateBuffer {
    fn default() -> Self {
        Self([0; 32 * 1024])
    }
}