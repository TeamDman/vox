use core::fmt;

use taxon::SemanticName;

use crate::bytes::{Sink, write_str, write_u8};

const STRING_TAG: u8 = 0x0f;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QualifiedName(SemanticName);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualifiedNameError {
    Empty,
    TooLong,
    TooFewLabels,
    InvalidLabel,
    ReservedNamespace,
}

impl QualifiedName {
    pub const MAX_BYTES: usize = 255;

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn application(value: &str) -> Result<Self, QualifiedNameError> {
        let name = Self::try_from(value)?;
        if name.as_str().starts_with("phon.") || name.as_str().starts_with("org.bearcove.phon.") {
            return Err(QualifiedNameError::ReservedNamespace);
        }
        Ok(name)
    }

    #[must_use]
    pub fn compact_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(4 + self.as_str().len());
        self.write_compact(&mut bytes);
        bytes
    }

    #[must_use]
    pub fn self_describing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(5 + self.as_str().len());
        write_u8(&mut bytes, STRING_TAG);
        self.write_compact(&mut bytes);
        bytes
    }

    pub(crate) fn write_compact<S: Sink>(&self, out: &mut S) {
        write_str(out, self.as_str());
    }
}

impl TryFrom<&str> for QualifiedName {
    type Error = QualifiedNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(QualifiedNameError::Empty);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(QualifiedNameError::TooLong);
        }
        let mut labels = 0usize;
        for label in value.split('.') {
            labels += 1;
            let bytes = label.as_bytes();
            if bytes.is_empty()
                || !bytes[0].is_ascii_lowercase()
                || bytes.last() == Some(&b'-')
                || bytes.iter().any(|byte| {
                    !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-'
                })
                || bytes.windows(2).any(|pair| pair == b"--")
            {
                return Err(QualifiedNameError::InvalidLabel);
            }
        }
        if labels < 2 {
            return Err(QualifiedNameError::TooFewLabels);
        }
        let semantic = SemanticName::try_from(value).map_err(|_| QualifiedNameError::TooLong)?;
        Ok(Self(semantic))
    }
}

impl TryFrom<SemanticName> for QualifiedName {
    type Error = QualifiedNameError;

    fn try_from(value: SemanticName) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl From<QualifiedName> for SemanticName {
    fn from(value: QualifiedName) -> Self {
        value.0
    }
}

pub fn from_compact_bytes(bytes: &[u8]) -> Result<QualifiedName, crate::DecodeError> {
    let mut reader = crate::Reader::new(bytes);
    let value = reader.read_str()?;
    if reader.remaining() != 0 {
        return Err(crate::DecodeError::TrailingBytes(reader.remaining()));
    }
    QualifiedName::try_from(value).map_err(|_| crate::DecodeError::Malformed("qualified name"))
}

pub fn from_self_describing_bytes(bytes: &[u8]) -> Result<QualifiedName, crate::DecodeError> {
    let mut reader = crate::Reader::new(bytes);
    let got = reader.read_u8()?;
    if got != STRING_TAG {
        return Err(crate::DecodeError::UnexpectedTag {
            expected: "qualified name string",
            got,
        });
    }
    let value = reader.read_str()?;
    if reader.remaining() != 0 {
        return Err(crate::DecodeError::TrailingBytes(reader.remaining()));
    }
    QualifiedName::try_from(value).map_err(|_| crate::DecodeError::Malformed("qualified name"))
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
