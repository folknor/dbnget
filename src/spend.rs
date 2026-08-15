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

/// Renders a dollar amount at enough precision to explain itself.
///
/// Two decimals is the natural way to write money and the wrong way to write this
/// number. The cost endpoint quotes list price to sub-cent precision - a day of
/// `ohlcv-1m` for one symbol prices at $0.000479 - so a refusal formatted at two
/// decimals reads "quoted $0.00 exceeds the --spend cap of $0.00", which states that
/// zero exceeds zero and tells the user nothing about what to pass instead.
///
/// The comparison stays exact; only the rendering widens. Precision grows until a
/// significant digit appears, so ordinary prices still read as `$12.34` and only the
/// amounts that need the extra digits pay for them. An exact zero is the one amount
/// `$0.00` describes perfectly, and no amount of precision would find a digit in it.
pub fn money(usd: f64) -> String {
    if !usd.is_finite() {
        return format!("${usd}");
    }
    if usd == 0.0 {
        return "$0.00".to_owned();
    }
    let significant = |s: &str| s.contains(|c: char| c.is_ascii_digit() && c != '0');

    // Two decimals is right whenever it says anything at all.
    let cents = format!("{usd:.2}");
    if significant(&cents) {
        return format!("${cents}");
    }

    // Below a cent, widen to the first precision that shows a digit and then one
    // further. The first digit alone rounds: $0.000479 renders as `$0.0005`, which is a
    // price the request does not have, in the direction that overstates it. A second
    // significant digit makes it `$0.00048`. Trailing zeroes earned by the extra digit
    // are dropped again, so `$0.0009` does not become `$0.00090`.
    for precision in 3..=MAX_PRECISION {
        if !significant(&format!("{usd:.precision$}")) {
            continue;
        }
        let widened = format!("{usd:.width$}", width = (precision + 1).min(MAX_PRECISION));
        return format!("${}", widened.trim_end_matches('0'));
    }
    // Smaller than the widest fixed rendering can show, but not zero: say so rather
    // than printing a row of zeroes that reads as free.
    format!("${usd:e}")
}

/// Where widening stops. Databento quotes to twelve places; past this the digits stop
/// telling a user anything they can act on, and `--spend` cannot express them anyway.
const MAX_PRECISION: usize = 8;

impl fmt::Display for Quote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} records, {} billable bytes, {}",
            self.records,
            self.billable_bytes,
            money(self.usd)
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
/// No `--spend` means a cap of $0.00, and that is a cap on the vendor's LIST PRICE, not
/// a prediction of the bill. The cost endpoint takes no account of the account: it
/// prices a request an active subscription covers identically to one nobody has paid
/// for, and it answers in sub-cent fractions. A request whose finished job later shows
/// `cost_usd: 0.0` quotes $0.000479 before it is submitted, so the default cap refuses
/// essentially every request that holds records.
///
/// That is deliberately not papered over. Rounding the comparison to cents would make
/// the default reachable by authorizing real sub-cent charges under a cap the user set
/// to zero, and the gate is not allowed to spend money nobody approved. There is no
/// third option available: nothing the API offers before a submit distinguishes
/// "covered" from "cheap", so a `--free` mode would have nothing to compute. Passing
/// `--spend` is the user's decision to make, not ours to round away.
///
/// The comparisons are written so a non-finite quote or cap cannot pass: NaN compares
/// false against everything, so `usd <= cap` would wave one straight through.
pub fn approve(quote: &Quote, spend: Option<f64>) -> Result<()> {
    if quote.is_empty() {
        bail!("this request matches no records - check the symbols, dataset and date range");
    }
    // A negative price is not a discount, it is a response nobody designed. The gate
    // fails closed on financial values it cannot account for, and a negative quote
    // would otherwise slip under every non-negative cap, including the $0.00 default.
    if !quote.usd.is_finite() || quote.usd < 0.0 {
        bail!(
            "the quoted cost is not a sensible amount ({}) - refusing",
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
                "this request is priced at {} ({quote}); pass --spend USD to approve the charge",
                money(quote.usd)
            );
        }
        bail!(
            "quoted {} exceeds the --spend cap of {}",
            money(quote.usd),
            money(cap)
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
        // A negative quote would otherwise pass every non-negative cap, the $0.00
        // default included.
        assert!(approve(&quote(-1.0, 100), None).is_err());
        assert!(approve(&quote(-1.0, 100), Some(10.0)).is_err());
    }

    /// The refusal has to say something a user can act on. Rendered at two decimals,
    /// the real quote for a day of one symbol's minute bars reads "$0.00 exceeds the
    /// cap of $0.00", which is both self-contradictory and silent about what `--spend`
    /// would let it through.
    #[test]
    fn a_sub_cent_price_is_rendered_at_a_precision_that_shows_it() {
        assert_eq!(money(0.000_479_400_158), "$0.00048");
        assert_eq!(money(0.0009), "$0.0009");
        assert_eq!(money(0.004), "$0.004");

        let err = approve(&quote(0.000_479_400_158, 766), Some(0.0))
            .unwrap_err()
            .to_string();
        assert!(err.contains("$0.00048"), "{err}");
        assert!(!err.contains("$0.00 exceeds"), "{err}");
    }

    /// Widening is for the amounts that need it. Everything else still reads as money.
    #[test]
    fn ordinary_amounts_keep_two_decimals() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(12.5), "$12.50");
        assert_eq!(money(1234.567), "$1234.57");
        assert_eq!(money(0.01), "$0.01");
    }

    /// A price too small for the widest fixed rendering is still not free, and must not
    /// be printed as a row of zeroes that reads as though it were.
    #[test]
    fn an_amount_below_the_widest_rendering_does_not_print_as_zero() {
        let rendered = money(1e-12);
        assert!(!rendered.contains("0.00000000"), "{rendered}");
        assert_ne!(rendered, "$0.00");
    }

    #[test]
    fn a_free_but_empty_request_is_refused() {
        assert!(approve(&quote(0.0, 0), Some(10.0)).is_err());
        assert!(approve(&quote(0.0, 0), None).is_err());
    }
}
