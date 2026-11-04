
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const HEADER_SIZE: usize = 20;
pub const TOMBSTONE: u32 = u32::MAX;

pub fn encode_record(
    key: &[u8],
    value: Option<&[u8]>,
    timestamp: u64,
) -> io::Result<Vec<u8>> {
    let key_len = u32::try_from(key.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "key is too large")
    })?;

    let val_len = match value {
        Some(v) => {
            let len = u32::try_from(v.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "value is too large")
            })?;

            if len == TOMBSTONE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "value length is reserved for tombstones",
                ));
            }

            len
        }
        None => TOMBSTONE,
    };

    let value_size = value.map_or(0, |v| v.len());

    let record_size = HEADER_SIZE
        .checked_add(key.len())
        .and_then(|n| n.checked_add(value_size))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "record is too large")
        })?;

    let mut buf = Vec::with_capacity(record_size);

    buf.extend_from_slice(&[0u8; 4]);
    buf.extend_from_slice(&timestamp.to_be_bytes());
    buf.extend_from_slice(&key_len.to_be_bytes());
    buf.extend_from_slice(&val_len.to_be_bytes());
    buf.extend_from_slice(key);

    if let Some(v) = value {
        buf.extend_from_slice(v);
    }

    let crc = crc32fast::hash(&buf[4..]);
    buf[..4].copy_from_slice(&crc.to_be_bytes());

    Ok(buf)
}

pub fn decode_header(buf: &[u8]) -> io::Result<(u32, u64, u32, u32)> {
    if buf.len() < HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "record header is too short",
        ));
    }

    let crc = u32::from_be_bytes(buf[0..4].try_into().expect("length checked"));
    let timestamp =
        u64::from_be_bytes(buf[4..12].try_into().expect("length checked"));
    let key_len =
        u32::from_be_bytes(buf[12..16].try_into().expect("length checked"));
    let val_len =
        u32::from_be_bytes(buf[16..20].try_into().expect("length checked"));

    Ok((crc, timestamp, key_len, val_len))
}

pub fn verify_record(record: &[u8]) -> io::Result<()> {
    if record.len() < HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "record is shorter than its header",
        ));
    }

    let (stored_crc, _, key_len, val_len) = decode_header(record)?;

    let value_len = if val_len == TOMBSTONE {
        0usize
    } else {
        val_len as usize
    };

    let expected_len = HEADER_SIZE
        .checked_add(key_len as usize)
        .and_then(|n| n.checked_add(value_len))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "record length overflow")
        })?;

    if record.len() != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record length does not match its header",
        ));
    }

    let actual_crc = crc32fast::hash(&record[4..]);

    if actual_crc != stored_crc {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record checksum mismatch",
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct LogPointer {
    pub offset: u64,
    pub val_len: u32,
    pub timestamp: u64,
}

pub struct Bitcask {
    file: File,
    write_offset: u64,
    keydir: HashMap<Vec<u8>, LogPointer>,
}

impl Bitcask {
    pub fn put(
        &mut self,
        key: &[u8],
        value: &[u8],
        timestamp: u64,
    ) -> io::Result<()> {
        let encoded = encode_record(key, Some(value), timestamp)?;

        let val_len = u32::try_from(value.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "value is too large")
        })?;

        let value_offset = self
            .write_offset
            .checked_add(HEADER_SIZE as u64)
            .and_then(|n| n.checked_add(key.len() as u64))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "file offset overflow")
            })?;

        let new_write_offset = self
            .write_offset
            .checked_add(encoded.len() as u64)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "file offset overflow")
            })?;

        self.file.seek(SeekFrom::Start(self.write_offset))?;

        if let Err(write_error) = self.file.write_all(&encoded) {
            // Best-effort rollback if a partial write occurred.
            if let Err(rollback_error) = self.file.set_len(self.write_offset) {
                return Err(io::Error::new(
                    rollback_error.kind(),
                    format!(
                        "write failed: {write_error}; rollback failed: {rollback_error}"
                    ),
                ));
            }

            return Err(write_error);
        }

        self.keydir.insert(
            key.to_vec(),
            LogPointer {
                offset: value_offset,
                val_len,
                timestamp,
            },
        );

        self.write_offset = new_write_offset;

        Ok(())
    }


    pub fn get(&mut self, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let pointer = match self.keydir.get(key) {
            Some(pointer) => *pointer,
            None => return Ok(None),
        };

        let value_len = pointer.val_len as usize;
        let mut value = vec![0u8; value_len];

        self.file.seek(SeekFrom::Start(pointer.offset))?;
        self.file.read_exact(&mut value)?;

        Ok(Some(value))
    }

    pub fn delete(&mut self, key: &[u8], timestamp: u64) -> io::Result<()> {
        let encoded = encode_record(key, None, timestamp)?;

        let new_write_offset = self
            .write_offset
            .checked_add(encoded.len() as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "file offset overflow",
                )
            })?;

        let original_offset = self.write_offset;

        self.file.seek(SeekFrom::Start(original_offset))?;

        if let Err(write_error) = self.file.write_all(&encoded) {
            if let Err(rollback_error) = self.file.set_len(original_offset) {
                return Err(io::Error::new(
                    rollback_error.kind(),
                    format!(
                        "tombstone write failed: {write_error}; \
                         rollback failed: {rollback_error}"
                    ),
                ));
            }

            return Err(write_error);
        }

        self.keydir.remove(key);
        self.write_offset = new_write_offset;

        Ok(())
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;

        let file_len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;

        let mut keydir = HashMap::new();
        let mut current_offset = 0u64;

        while current_offset < file_len {
            let mut header_buf = [0u8; HEADER_SIZE];

            match file.read_exact(&mut header_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(e) => return Err(e),
            }

            let (_crc, timestamp, key_len, val_len) =
                decode_header(&header_buf)?;

            let value_len = if val_len == TOMBSTONE {
                0usize
            } else {
                val_len as usize
            };

            let record_len = HEADER_SIZE
                .checked_add(key_len as usize)
                .and_then(|n| n.checked_add(value_len))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "record length overflow",
                    )
                })?;

            let remaining = file_len - current_offset;

            if record_len as u64 > remaining {
                break;
            }

            let mut record_buf = Vec::new();

            record_buf.try_reserve_exact(record_len).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    format!("failed to allocate record buffer: {e}"),
                )
            })?;

            record_buf.extend_from_slice(&header_buf);

            record_buf.resize(record_len, 0);

            if let Err(e) = file.read_exact(&mut record_buf[HEADER_SIZE..]) {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e);
            }

            verify_record(&record_buf)?;

            let key_start = HEADER_SIZE;
            let key_end = key_start + key_len as usize;
            let key = record_buf[key_start..key_end].to_vec();

            if val_len == TOMBSTONE {
                keydir.remove(&key);
            } else {
                let value_offset = current_offset
                    + HEADER_SIZE as u64
                    + key_len as u64;

                keydir.insert(
                    key,
                    LogPointer {
                        offset: value_offset,
                        val_len,
                        timestamp,
                    },
                );
            }

            current_offset += record_len as u64;
        }

        if current_offset < file_len {
            file.set_len(current_offset)?;
        }

        file.seek(SeekFrom::End(0))?;

        Ok(Self {
            file,
            write_offset: current_offset,
            keydir,
        })
    }

}
