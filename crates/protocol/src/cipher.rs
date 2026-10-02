//! AES-128/CFB8 stream encryption, keyed and IV'd with the shared secret
//! (`Crypt.getCipher`).

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};

pub struct Encryptor(cfb8::Encryptor<Aes128>);
pub struct Decryptor(cfb8::Decryptor<Aes128>);

impl Encryptor {
    pub fn new(secret: &[u8; 16]) -> Self {
        Self(cfb8::Encryptor::new(secret.into(), secret.into()))
    }

    pub fn encrypt(&mut self, data: &mut [u8]) {
        // CFB8 has a one-byte block, so each byte is one block.
        for b in data {
            self.0.encrypt_block_mut(GenericArray::from_mut_slice(std::slice::from_mut(b)));
        }
    }
}

impl Decryptor {
    pub fn new(secret: &[u8; 16]) -> Self {
        Self(cfb8::Decryptor::new(secret.into(), secret.into()))
    }

    pub fn decrypt(&mut self, data: &mut [u8]) {
        for b in data {
            self.0.decrypt_block_mut(GenericArray::from_mut_slice(std::slice::from_mut(b)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_state_carries_across_calls() {
        let key = [7u8; 16];
        let msg: Vec<u8> = (0..100).collect();

        let mut whole = msg.clone();
        Encryptor::new(&key).encrypt(&mut whole);

        let mut split = msg.clone();
        let mut enc = Encryptor::new(&key);
        let (a, b) = split.split_at_mut(37);
        enc.encrypt(a);
        enc.encrypt(b);
        assert_eq!(whole, split);

        let mut dec = Decryptor::new(&key);
        dec.decrypt(&mut split);
        assert_eq!(split, msg);
    }
}
