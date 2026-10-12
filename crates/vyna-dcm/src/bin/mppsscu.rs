//! `mppsscu`: Modality Performed Procedure Step N-CREATE + N-SET.

#[path = "common.rs"]
mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use common::{ConnArgs, anyhowish::Result};
use vyna_dcm::core::sop_class;
use vyna_dcm::net::worklist::MppsData;

/// MPPS SCU: N-CREATE (IN PROGRESS) then N-SET (COMPLETED/DISCONTINUED).
#[derive(Parser, Debug)]
#[command(name = "mppsscu", about = "Send an MPPS N-CREATE followed by N-SET")]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// MPPS SOP Instance UID (generated when omitted).
    #[arg(long)]
    mpps_uid: Option<String>,
    /// Performed Procedure Step ID.
    #[arg(long)]
    pps_id: String,
    /// Performed station AE title (defaults to our calling AE title).
    #[arg(long)]
    station_ae: Option<String>,
    /// Performed station name.
    #[arg(long)]
    station_name: Option<String>,
    /// Modality of the performed step (e.g. ECG, CT).
    #[arg(long)]
    modality: String,
    /// Start date `YYYYMMDD` (defaults to today, UTC).
    #[arg(long)]
    start_date: Option<String>,
    /// Start time `HHMMSS` (defaults to now, UTC).
    #[arg(long)]
    start_time: Option<String>,
    /// End date `YYYYMMDD` for N-SET (defaults to today, UTC).
    #[arg(long)]
    end_date: Option<String>,
    /// End time `HHMMSS` for N-SET (defaults to now, UTC).
    #[arg(long)]
    end_time: Option<String>,
    /// Scheduled step linkage: Study Instance UID.
    #[arg(long)]
    study_uid: Option<String>,
    /// Scheduled step linkage: Accession Number.
    #[arg(long)]
    accession: Option<String>,
    /// Scheduled step linkage: Requested Procedure ID.
    #[arg(long)]
    req_proc_id: Option<String>,
    /// Scheduled step linkage: Scheduled Procedure Step ID.
    #[arg(long)]
    sps_id: Option<String>,
    /// Performed series as SERIES_UID:CLASS_UID:INST_UID (N-SET). Repeatable.
    #[arg(long = "series")]
    series: Vec<String>,
    /// Final status DISCONTINUED instead of COMPLETED.
    #[arg(long, default_value_t = false)]
    discontinue: bool,
}

/// Current UTC date/time as (`YYYYMMDD`, `HHMMSS`).
fn utc_now() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let sod = secs % 86400;
    (
        format!("{y:04}{m:02}{d:02}"),
        format!("{:02}{:02}{:02}", sod / 3600, (sod % 3600) / 60, sod % 60),
    )
}

/// Days since 1970-01-01 → (year, month, day); Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_series(s: &str) -> Result<(String, String, String)> {
    let mut parts = s.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c), None) if !a.is_empty() && !b.is_empty() && !c.is_empty() => {
            Ok((a.to_string(), b.to_string(), c.to_string()))
        }
        _ => Err(common::anyhowish::Error::msg(format!(
            "series {s:?} must be SERIES_UID:CLASS_UID:INST_UID"
        ))),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mpps_uid = args
        .mpps_uid
        .clone()
        .unwrap_or_else(vyna_dcm::core::new_uid);
    let (today, now) = utc_now();
    let mut performed_series = Vec::with_capacity(args.series.len());
    for s in &args.series {
        performed_series.push(parse_series(s)?);
    }
    let final_status = if args.discontinue {
        "DISCONTINUED"
    } else {
        "COMPLETED"
    };

    let mut data = MppsData {
        performed_procedure_step_id: args.pps_id.clone(),
        performed_station_ae: args
            .station_ae
            .clone()
            .unwrap_or_else(|| args.conn.calling.clone()),
        performed_station_name: args.station_name.clone(),
        modality: args.modality.clone(),
        start_date: args.start_date.clone().unwrap_or_else(|| today.clone()),
        start_time: args.start_time.clone().unwrap_or_else(|| now.clone()),
        status: "IN PROGRESS".to_string(),
        study_instance_uid: args.study_uid.clone(),
        accession_number: args.accession.clone(),
        requested_procedure_id: args.req_proc_id.clone(),
        scheduled_procedure_step_id: args.sps_id.clone(),
        performed_series,
        ..Default::default()
    };

    let sop = sop_class::MODALITY_PERFORMED_PROCEDURE_STEP;
    let mut assoc = args
        .conn
        .connect(&[sop], &ConnArgs::default_syntaxes())
        .await?;
    let (cs, _) = assoc
        .n_create(sop, &mpps_uid, Some(&data.to_create_dataset()))
        .await?;
    println!("N-CREATE {mpps_uid}: status {cs:#06X}");
    data.status = final_status.to_string();
    data.end_date = Some(args.end_date.clone().unwrap_or_else(|| today.clone()));
    data.end_time = Some(args.end_time.clone().unwrap_or_else(|| now.clone()));
    let (ss, _) = assoc.n_set(sop, &mpps_uid, &data.to_set_dataset()).await?;
    println!("N-SET {mpps_uid} ({final_status}): status {ss:#06X}");
    assoc.release().await?;
    if cs != 0 || ss != 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "MPPS failed: N-CREATE {cs:#06X}, N-SET {ss:#06X}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        assert_eq!(civil_from_days(19723), (2024, 1, 1));
    }

    #[test]
    fn utc_now_has_dicom_shapes() {
        let (d, t) = utc_now();
        assert_eq!(d.len(), 8);
        assert_eq!(t.len(), 6);
        assert!(d.bytes().all(|b| b.is_ascii_digit()));
        assert!(t.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn series_triples_parse() {
        assert_eq!(
            parse_series("1.2:1.3:1.4").unwrap(),
            ("1.2".to_string(), "1.3".to_string(), "1.4".to_string())
        );
        assert!(parse_series("1.2:1.3").is_err());
        assert!(parse_series("1.2::1.4").is_err());
        assert!(parse_series("1.2:1.3:1.4:1.5").is_err());
    }
}
