//! Private authenticated pagination positions; no cursor body grants authority.

use aws_lc_rs::aead::{Aad, Nonce, RandomizedNonceKey, AES_256_GCM_SIV};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CursorError {
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("cursor exceeds its configured bound")]
    Limit,
    #[error("cursor encryption failed")]
    Crypto,
}

#[derive(Clone, Copy)]
pub(crate) struct CursorBinding<'a> {
    pub domain: &'a str,
    pub subject: &'a str,
    pub lease: &'a str,
    pub query_digest: &'a str,
}

pub(crate) struct CursorCodec {
    key: RandomizedNonceKey,
    max_plaintext_bytes: usize,
    max_frame_bytes: usize,
    max_encoded_bytes: usize,
}

impl CursorCodec {
    pub(crate) fn new(key: &[u8; 32], max_key_bytes: usize) -> Result<Self, CursorError> {
        if max_key_bytes == 0 {
            return Err(CursorError::Limit);
        }
        let max_plaintext_bytes = max_key_bytes.checked_mul(4).ok_or(CursorError::Limit)?;
        let max_frame_bytes = max_plaintext_bytes
            .checked_add(AES_256_GCM_SIV.nonce_len())
            .and_then(|length| length.checked_add(AES_256_GCM_SIV.tag_len()))
            .ok_or(CursorError::Limit)?;
        let max_encoded_bytes =
            base64::encoded_len(max_frame_bytes, false).ok_or(CursorError::Limit)?;
        // JSON can escape each input byte with six ASCII bytes, plus tuple syntax.
        max_plaintext_bytes
            .checked_mul(6)
            .and_then(|value| value.checked_add(13))
            .ok_or(CursorError::Limit)?;
        Ok(Self {
            key: RandomizedNonceKey::new(&AES_256_GCM_SIV, key).map_err(|_| CursorError::Crypto)?,
            max_plaintext_bytes,
            max_frame_bytes,
            max_encoded_bytes,
        })
    }

    fn aad(&self, binding: CursorBinding<'_>) -> Result<Vec<u8>, CursorError> {
        [
            binding.domain,
            binding.subject,
            binding.lease,
            binding.query_digest,
        ]
        .into_iter()
        .try_fold(0usize, |length, field| {
            length
                .checked_add(field.len())
                .filter(|length| *length <= self.max_plaintext_bytes)
        })
        .ok_or(CursorError::Limit)?;
        serde_json::to_vec(&(
            binding.domain,
            binding.subject,
            binding.lease,
            binding.query_digest,
        ))
        .map_err(|_| CursorError::Crypto)
    }

    pub(crate) fn seal(
        &self,
        plaintext: &str,
        binding: CursorBinding<'_>,
    ) -> Result<String, CursorError> {
        if plaintext.len() > self.max_plaintext_bytes {
            return Err(CursorError::Limit);
        }
        let aad = self.aad(binding)?;
        let mut ciphertext = Vec::with_capacity(plaintext.len() + AES_256_GCM_SIV.tag_len());
        ciphertext.extend_from_slice(plaintext.as_bytes());
        // The crypto library owns random nonce generation. Persisting a counter
        // here would couple cursor issuance to another durable write protocol.
        let nonce = self
            .key
            .seal_in_place_append_tag(Aad::from(aad), &mut ciphertext)
            .map_err(|_| CursorError::Crypto)?;
        let mut frame = Vec::with_capacity(nonce.as_ref().len() + ciphertext.len());
        frame.extend_from_slice(nonce.as_ref());
        frame.extend_from_slice(&ciphertext);
        Ok(URL_SAFE_NO_PAD.encode(frame))
    }

    pub(crate) fn open(
        &self,
        encoded: &str,
        binding: CursorBinding<'_>,
    ) -> Result<String, CursorError> {
        if encoded.len() > self.max_encoded_bytes {
            return Err(CursorError::Limit);
        }
        if encoded.len() % 4 == 1 {
            return Err(CursorError::InvalidCursor);
        }
        let decoded_length = encoded.len() / 4 * 3 + encoded.len() % 4 * 6 / 8;
        let nonce_length = AES_256_GCM_SIV.nonce_len();
        if decoded_length < nonce_length + AES_256_GCM_SIV.tag_len() {
            return Err(CursorError::InvalidCursor);
        }
        if decoded_length > self.max_frame_bytes {
            return Err(CursorError::Limit);
        }
        let aad = self.aad(binding)?;
        let mut frame = vec![0u8; decoded_length];
        let written = URL_SAFE_NO_PAD
            .decode_slice(encoded, &mut frame)
            .map_err(|_| CursorError::InvalidCursor)?;
        if written != decoded_length {
            return Err(CursorError::InvalidCursor);
        }
        let nonce = Nonce::try_assume_unique_for_key(&frame[..nonce_length])
            .map_err(|_| CursorError::InvalidCursor)?;
        let plaintext = self
            .key
            .open_in_place(nonce, Aad::from(aad), &mut frame[nonce_length..])
            .map_err(|_| CursorError::InvalidCursor)?;
        std::str::from_utf8(plaintext)
            .map(str::to_owned)
            .map_err(|_| CursorError::InvalidCursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> CursorBinding<'static> {
        CursorBinding {
            domain: "trace/reader/forward",
            subject: "subject-a",
            lease: "exact-view-lease",
            query_digest: "query-and-current-grant-digest",
        }
    }

    #[test]
    fn cursor_ciphertexts_hide_positions_and_use_independent_random_nonces() {
        let codec = CursorCodec::new(&[7; 32], 128).unwrap();
        let position = "private-hidden-environment:private-child-position";
        let first = codec.seal(position, binding()).unwrap();
        let second = codec.seal(position, binding()).unwrap();
        assert_ne!(first, second);
        let first_bytes = URL_SAFE_NO_PAD.decode(&first).unwrap();
        let second_bytes = URL_SAFE_NO_PAD.decode(&second).unwrap();
        let nonce_length = AES_256_GCM_SIV.nonce_len();
        assert_ne!(&first_bytes[..nonce_length], &second_bytes[..nonce_length]);
        assert!(!first_bytes
            .windows(position.len())
            .any(|part| part == position.as_bytes()));
        for token in [first, second] {
            assert_eq!(codec.open(&token, binding()).unwrap(), position);
        }
    }

    #[test]
    fn cursor_nonce_ciphertext_and_tag_tampering_never_returns_a_position() {
        let codec = CursorCodec::new(&[7; 32], 128).unwrap();
        let token = codec.seal("private-position", binding()).unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(token).unwrap();
        for offset in [0, AES_256_GCM_SIV.nonce_len(), bytes.len() - 1] {
            let mut damaged = bytes.clone();
            damaged[offset] ^= 1;
            assert_eq!(
                codec.open(&URL_SAFE_NO_PAD.encode(damaged), binding()),
                Err(CursorError::InvalidCursor)
            );
        }
        let wrong_key = CursorCodec::new(&[8; 32], 128).unwrap();
        assert_eq!(
            wrong_key.open(&URL_SAFE_NO_PAD.encode(bytes), binding()),
            Err(CursorError::InvalidCursor)
        );
    }

    #[test]
    fn cursor_authentication_binds_domain_subject_exact_lease_and_query_grants() {
        let codec = CursorCodec::new(&[7; 32], 128).unwrap();
        let token = codec.seal("position", binding()).unwrap();
        for changed in [
            CursorBinding {
                domain: "trace/reader/backward",
                ..binding()
            },
            CursorBinding {
                subject: "subject-b",
                ..binding()
            },
            CursorBinding {
                lease: "another-view-lease",
                ..binding()
            },
            CursorBinding {
                query_digest: "changed-query-or-current-grants",
                ..binding()
            },
        ] {
            assert_eq!(codec.open(&token, changed), Err(CursorError::InvalidCursor));
        }
        let first = CursorBinding {
            subject: "a",
            lease: "bc",
            ..binding()
        };
        let second = CursorBinding {
            subject: "ab",
            lease: "c",
            ..binding()
        };
        let token = codec.seal("position", first).unwrap();
        assert_eq!(codec.open(&token, second), Err(CursorError::InvalidCursor));
    }

    #[test]
    fn cursor_plaintext_wire_and_binding_allocations_have_configured_bounds() {
        let codec = CursorCodec::new(&[7; 32], 32).unwrap();
        let maximum = "x".repeat(codec.max_plaintext_bytes);
        let token = codec.seal(&maximum, binding()).unwrap();
        assert_eq!(token.len(), codec.max_encoded_bytes);
        assert_eq!(codec.open(&token, binding()).unwrap(), maximum);
        assert_eq!(
            codec.seal(&(maximum + "x"), binding()),
            Err(CursorError::Limit)
        );
        assert_eq!(
            codec.open(&"!".repeat(codec.max_encoded_bytes + 1), binding()),
            Err(CursorError::Limit)
        );
        let oversized = "x".repeat(codec.max_plaintext_bytes + 1);
        let binding = CursorBinding {
            lease: &oversized,
            ..binding()
        };
        assert_eq!(codec.seal("position", binding), Err(CursorError::Limit));
        assert_eq!(codec.open(&token, binding), Err(CursorError::Limit));
        assert!(matches!(
            CursorCodec::new(&[7; 32], 0),
            Err(CursorError::Limit)
        ));
        assert!(matches!(
            CursorCodec::new(&[7; 32], usize::MAX),
            Err(CursorError::Limit)
        ));
    }

    #[test]
    fn malformed_cursor_frames_are_rejected_before_a_position_is_returned() {
        let codec = CursorCodec::new(&[7; 32], 128).unwrap();
        for token in ["", "a", "not base64!", "AA", "AAAA", "===="] {
            assert_eq!(
                codec.open(token, binding()),
                Err(CursorError::InvalidCursor)
            );
        }
        let mut ciphertext = vec![0xff];
        let nonce = codec
            .key
            .seal_in_place_append_tag(Aad::from(codec.aad(binding()).unwrap()), &mut ciphertext)
            .unwrap();
        let token =
            URL_SAFE_NO_PAD.encode([nonce.as_ref().as_slice(), ciphertext.as_slice()].concat());
        assert_eq!(
            codec.open(&token, binding()),
            Err(CursorError::InvalidCursor)
        );
    }
}
