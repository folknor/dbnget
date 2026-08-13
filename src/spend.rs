//! Pricing a request, and deciding whether it is allowed to be paid for.

use std::fmt;

use anyhow::{Context, Result, bail};
use databento::{HistoricalClient, historical::metadata::GetQueryParams};

/// What the metadata endpoints say a request would cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    pub records: u64,
    pub billable_bytes: u64,
    pub usd: f64,
}

impl Quote {
    /// Whether a zero price means "already covered" or "this request matches nothing".
    ///
    /// Both quote at $0.00, and the difference matters: the first is free data, the
    /// second is a symbology or date-range mistake that would otherwise be submitted
    /// happily and deliver an empty file.
    pub fn is_empty(&self) -> bool {
        self.records == 0
    }
}

impl fmt::Display for Quote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} records, {} billable bytes, ${:.2}",
            self.records, self.billable_bytes, self.usd
        )?;
        if self.is_empty() {
            f.write_str(" (EMPTY - matches no records, not free)")?;
        }
        Ok(())
    }
}

/// Prices a request. Always a live call: a submission must never be gated on a cached
/// number, because the number is the thing being agreed to.
pub async fn fetch(client: &mut HistoricalClient, params: &GetQueryParams) -> Result<Quote> {
    let records = client
        .metadata()
        .get_record_count(params)
        .await
        .context("fetching record count")?;
    let billable_bytes = client
        .metadata()
        .get_billable_size(params)
        .await
        .context("fetching billable size")?;
    let usd = client
        .metadata()
        .get_cost(params)
        .await
        .context("fetching cost")?;
    Ok(Quote {
        records,
        billable_bytes,
        usd,
    })
}

/// Refuses anything `--spend` does not clearly permit.
///
/// No `--spend` means a cap of $0.00: on a subscription a covered request quotes
/// exactly zero, so the default is "fetch only what I have already paid for". The
/// comparisons are written so a non-finite quote or cap cannot pass: NaN compares
/// false against everything, so `usd <= cap` would wave one straight through.
pub fn approve(quote: &Quote, spend: Option<f64>) -> Result<()> {
    if quote.is_empty() {
        bail!("this request matches no records - check the symbols, dataset and date range");
    }
    if !quote.usd.is_finite() {
        bail!(
            "the quoted cost is not a finite number ({}) - refusing",
            quote.usd
        );
    }
    let cap = spend.unwrap_or(0.0);
    if !cap.is_finite() || cap < 0.0 {
        bail!("--spend must be a finite, non-negative number, not {cap}");
    }
    if quote.usd > cap {
        if spend.is_none() {
            bail!(
                "this request costs ${:.2} ({quote}); pass --spend USD to approve the charge",
                quote.usd
            );
        }
        bail!(
            "quoted ${:.2} exceeds the --spend cap of ${cap:.2}",
            quote.usd
        );
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    fn quote(usd: f64, records: u64) -> Quote {
        Quote {
            records,
            billable_bytes: 1024,
            usd,
        }
    }

    #[test]
    fn a_quote_under_the_cap_is_approved() {
        approve(&quote(5.0, 100), Some(10.0)).unwrap();
        approve(&quote(10.0, 100), Some(10.0)).unwrap();
    }

    #[test]
    fn a_quote_over_the_cap_is_refused() {
        assert!(approve(&quote(10.01, 100), Some(10.0)).is_err());
    }

    #[test]
    fn no_spend_flag_means_a_zero_cap() {
        approve(&quote(0.0, 100), None).unwrap();
        assert!(approve(&quote(0.01, 100), None).is_err());
    }

    #[test]
    fn non_finite_numbers_cannot_slip_past_the_cap() {
        assert!(approve(&quote(f64::NAN, 100), Some(10.0)).is_err());
        assert!(approve(&quote(f64::INFINITY, 100), Some(10.0)).is_err());
        assert!(approve(&quote(1.0, 100), Some(f64::NAN)).is_err());
        assert!(approve(&quote(1.0, 100), Some(f64::INFINITY)).is_err());
    }

    #[test]
    fn a_free_but_empty_request_is_refused() {
        assert!(approve(&quote(0.0, 0), Some(10.0)).is_err());
        assert!(approve(&quote(0.0, 0), None).is_err());
    }
}
