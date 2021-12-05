use std::ops::{Deref, DerefMut};

use rkyv::AlignedVec;
#[cfg(feature = "sled")]
use sled::IVec;

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum EVec {
    #[cfg(feature = "sled")]
    Inline(IVec),
    Owned(Vec<u8>),
}

impl Default for EVec {
    fn default() -> Self {
        EVec::Owned(Vec::new())
    }
}

impl From<EVec> for Vec<u8> {
    fn from(evec: EVec) -> Self {
        match evec {
            #[cfg(feature = "sled")]
            EVec::Inline(inline) => inline.to_vec(),
            EVec::Owned(owned) => owned,
        }
    }
}

impl From<AlignedVec> for EVec {
    fn from(avec: AlignedVec) -> Self {
        EVec::Owned(avec.into())
    }
}

#[cfg(feature = "sled")]
impl From<IVec> for EVec {
    fn from(ivec: IVec) -> Self {
        EVec::Inline(ivec)
    }
}

#[cfg(feature = "sled")]
impl From<EVec> for IVec {
    fn from(evec: EVec) -> Self {
        match evec {
            EVec::Inline(ivec) => ivec,
            EVec::Owned(vec) => vec.into(),
        }
    }
}

impl From<Vec<u8>> for EVec {
    fn from(vec: Vec<u8>) -> Self {
        EVec::Owned(vec)
    }
}

impl From<&[u8]> for EVec {
    fn from(v: &[u8]) -> Self {
        EVec::Owned(v.to_vec())
    }
}

impl<const N: usize> From<[u8; N]> for EVec {
    fn from(arr: [u8; N]) -> Self {
        EVec::Owned(arr.to_vec())
    }
}

impl AsRef<[u8]> for EVec {
    fn as_ref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "sled")]
            EVec::Inline(inline) => inline.as_ref(),
            EVec::Owned(owned) => owned.as_slice(),
        }
    }
}

impl AsMut<[u8]> for EVec {
    fn as_mut(&mut self) -> &mut [u8] {
        match self {
            #[cfg(feature = "sled")]
            EVec::Inline(inline) => inline.as_mut(),
            EVec::Owned(owned) => owned.as_mut_slice(),
        }
    }
}

impl std::borrow::Borrow<[u8]> for EVec {
    fn borrow(&self) -> &[u8] {
        self.as_ref()
    }
}

impl std::borrow::Borrow<[u8]> for &EVec {
    fn borrow(&self) -> &[u8] {
        self.as_ref()
    }
}

impl Deref for EVec {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl DerefMut for EVec {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

impl<const N: usize> TryFrom<EVec> for [u8; N] {
    type Error = EVec;

    fn try_from(v: EVec) -> Result<[u8; N], Self::Error> {
        match v {
            #[cfg(feature = "sled")]
            EVec::Inline(ivec) => ivec.as_ref().try_into().map_err(|_| EVec::Inline(ivec)),
            EVec::Owned(vec) => vec.try_into().map_err(EVec::Owned),
        }
    }
}
