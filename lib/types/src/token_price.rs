use chrono::{DateTime, Utc};
use num::rational::Ratio;
use num::{BigInt, BigUint};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TokenPriceError {
    #[error("token price must be a positive finite number, got {0}")]
    NonPositiveOrNonFinite(f64),
    #[error("failed to convert token price {0} to ratio")]
    RatioConversion(f64),
}

/// Struct to represent API response containing ratio and timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenApiRatio {
    /// Ratio representing the USD price of the token unit.
    pub ratio: Ratio<BigUint>,
    /// Either the timestamp of the quote or the timestamp of the request.
    pub timestamp: DateTime<Utc>,
}

impl TokenApiRatio {
    pub fn from_f64_decimals_and_timestamp(
        value: f64,
        decimals: u8,
        timestamp: Option<DateTime<Utc>>,
    ) -> Self {
        Self::try_from_f64_decimals_and_timestamp(value, decimals, timestamp)
            .expect("invalid token price")
    }

    // SYSCOIN: External price APIs are untrusted inputs. Keep validation fallible so malformed
    // prices are handled by callers' retry/error paths instead of crashing the main node.
    pub fn try_from_f64_decimals_and_timestamp(
        value: f64,
        decimals: u8,
        timestamp: Option<DateTime<Utc>>,
    ) -> Result<Self, TokenPriceError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(TokenPriceError::NonPositiveOrNonFinite(value));
        }
        let signed_ratio =
            Ratio::<BigInt>::from_float(value).ok_or(TokenPriceError::RatioConversion(value))?;

        let mut ratio = Ratio::<BigUint>::new(
            signed_ratio
                .numer()
                .to_biguint()
                .ok_or(TokenPriceError::RatioConversion(value))?,
            signed_ratio
                .denom()
                .to_biguint()
                .ok_or(TokenPriceError::RatioConversion(value))?,
        );
        ratio /= BigUint::from(10u64).pow(decimals as u32);

        Ok(Self {
            ratio,
            timestamp: timestamp.unwrap_or_else(Utc::now),
        })
    }

    pub fn reciprocal(&self) -> Self {
        Self {
            ratio: self.ratio.recip(),
            timestamp: self.timestamp,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenPricesForFees {
    pub base_token_usd_price: TokenApiRatio,
    pub sl_token_usd_price: TokenApiRatio,
}

#[cfg(test)]
mod tests {
    use super::{TokenApiRatio, TokenPriceError};

    #[test]
    fn fallible_constructor_rejects_malformed_prices() {
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = TokenApiRatio::try_from_f64_decimals_and_timestamp(value, 18, None)
                .expect_err("malformed price should be rejected");
            assert!(matches!(err, TokenPriceError::NonPositiveOrNonFinite(_)));
        }
    }

    #[test]
    fn fallible_constructor_accepts_positive_finite_price() {
        let ratio = TokenApiRatio::try_from_f64_decimals_and_timestamp(1.25, 18, None)
            .expect("valid price should convert");
        assert!(*ratio.ratio.numer() > 0u32.into());
    }
}
