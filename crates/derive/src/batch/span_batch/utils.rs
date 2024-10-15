//! Utilities for Span Batch Encoding and Decoding.

use super::{SpanBatchError, SpanDecodingError};
use alloc::vec::Vec;
use alloy_consensus::{TxEnvelope, TxType};
use alloy_rlp::{Buf, Header};

/// Reads transaction data from a reader.
pub(crate) fn read_tx_data(r: &mut &[u8]) -> Result<(Vec<u8>, TxType), SpanBatchError> {
    let mut tx_data = Vec::new();
    let first_byte =
        *r.first().ok_or(SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionData))?;
    let mut tx_type = 0;
    if first_byte <= 0x7F {
        // EIP-2718: Non-legacy tx, so write tx type
        tx_type = first_byte;
        tx_data.push(tx_type);
        r.advance(1);
    }

    // Read the RLP header with a different reader pointer. This prevents the initial pointer from
    // being advanced in the case that what we read is invalid.
    let rlp_header = Header::decode(&mut (**r).as_ref())
        .map_err(|_| SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionData))?;

    let tx_payload = if rlp_header.list {
        // Grab the raw RLP for the transaction data from `r`. It was unaffected since we copied it.
        let payload_length_with_header = rlp_header.payload_length + rlp_header.length();
        let payload = r[0..payload_length_with_header].to_vec();
        r.advance(payload_length_with_header);
        Ok(payload)
    } else {
        Err(SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionData))
    }?;
    tx_data.extend_from_slice(&tx_payload);

    Ok((
        tx_data,
        tx_type
            .try_into()
            .map_err(|_| SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionType))?,
    ))
}

/// Converts a `v` value to a y parity bit, from the transaaction type.
pub(crate) const fn convert_v_to_y_parity(v: u64, tx_type: TxType) -> Result<bool, SpanBatchError> {
    match tx_type {
        TxType::Legacy => {
            if v != 27 && v != 28 {
                // EIP-155: v = 2 * chain_id + 35 + yParity
                Ok((v - 35) & 1 == 1)
            } else {
                // Unprotected legacy txs must have v = 27 or 28
                Ok(v - 27 == 1)
            }
        }
        TxType::Eip2930 | TxType::Eip1559 => Ok(v == 1),
        _ => Err(SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionType)),
    }
}

/// Checks if the signature of the passed [TxEnvelope] is protected.
pub(crate) const fn is_protected_v(tx: &TxEnvelope) -> bool {
    match tx {
        TxEnvelope::Legacy(tx) => {
            let v = tx.signature().v().to_u64();
            if 64 - v.leading_zeros() <= 8 {
                return v != 27 && v != 28 && v != 1 && v != 0;
            }
            // anything not 27 or 28 is considered protected
            true
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{
        Signed, TxEip1559, TxEip2930, TxEip4844, TxEip4844Variant, TxEip7702, TxLegacy,
    };
    use alloy_primitives::{b256, Signature};

    #[test]
    fn test_convert_v_to_y_parity() {
        assert_eq!(convert_v_to_y_parity(27, TxType::Legacy), Ok(false));
        assert_eq!(convert_v_to_y_parity(28, TxType::Legacy), Ok(true));
        assert_eq!(convert_v_to_y_parity(36, TxType::Legacy), Ok(true));
        assert_eq!(convert_v_to_y_parity(37, TxType::Legacy), Ok(false));
        assert_eq!(convert_v_to_y_parity(1, TxType::Eip2930), Ok(true));
        assert_eq!(convert_v_to_y_parity(1, TxType::Eip1559), Ok(true));
        assert_eq!(
            convert_v_to_y_parity(1, TxType::Eip4844),
            Err(SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionType))
        );
        assert_eq!(
            convert_v_to_y_parity(0, TxType::Eip7702),
            Err(SpanBatchError::Decoding(SpanDecodingError::InvalidTransactionType))
        );
    }

    #[test]
    fn test_is_protected_v() {
        let sig = Signature::test_signature();
        assert!(!is_protected_v(&TxEnvelope::Legacy(Signed::new_unchecked(
            TxLegacy::default(),
            sig,
            Default::default(),
        ))));
        let r = b256!("840cfc572845f5786e702984c2a582528cad4b49b2a10b9db1be7fca90058565");
        let s = b256!("25e7109ceb98168d95b09b18bbf6b685130e0562f233877d492b94eee0c5b6d1");
        let v = 27;
        let valid_sig = Signature::from_scalars_and_parity(r, s, v).unwrap();
        assert!(!is_protected_v(&TxEnvelope::Legacy(Signed::new_unchecked(
            TxLegacy::default(),
            valid_sig,
            Default::default(),
        ))));
        assert!(is_protected_v(&TxEnvelope::Eip2930(Signed::new_unchecked(
            TxEip2930::default(),
            sig,
            Default::default(),
        ))));
        assert!(is_protected_v(&TxEnvelope::Eip1559(Signed::new_unchecked(
            TxEip1559::default(),
            sig,
            Default::default(),
        ))));
        assert!(is_protected_v(&TxEnvelope::Eip4844(Signed::new_unchecked(
            TxEip4844Variant::TxEip4844(TxEip4844::default()),
            sig,
            Default::default(),
        ))));
        assert!(is_protected_v(&TxEnvelope::Eip7702(Signed::new_unchecked(
            TxEip7702::default(),
            sig,
            Default::default(),
        ))));
    }
}