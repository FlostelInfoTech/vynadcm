//! VynaECG pattern: build a 12-lead ECG recording, write a Part-10 file,
//! read it back and verify the samples.
//!
//! Run: `cargo run -p vyna-dcm --example ecg_write`

use vyna_dcm::ecg::{
    EcgInstanceMeta, EcgLead, EcgRecording, STANDARD_12_LEAD, recording_from_waveform_item,
    recording_to_dataset,
};
use vyna_dcm::{FileMeta, PersonName, TransferSyntax, tags};

fn main() -> Result<(), String> {
    // 1. Synthesize 10 s of 12-lead ECG at 500 Hz (tall T-ish sine + QRS
    //    spikes stand in for real acquisition samples).
    let mut leads = Vec::new();
    for (i, (label, code)) in STANDARD_12_LEAD.iter().enumerate() {
        let mut lead = EcgLead::new(
            *label,
            (0..5000)
                .map(|t| {
                    let qrs = if t % 500 < 20 { 1200 } else { 0 };
                    let base =
                        ((t as f64 / 500.0 * 2.0 * std::f64::consts::PI * 1.2).sin() * 80.0) as i32;
                    base + qrs + i as i32 * 3
                })
                .collect(),
            0.001, // 1 µV per LSB, reported in mV
        );
        lead.mdc_code = Some((*code).to_string());
        lead.filter_low_hz = Some(0.05);
        lead.filter_high_hz = Some(150.0);
        leads.push(lead);
    }
    let rec = EcgRecording::new(leads, 500.0)?;
    println!(
        "recording: {} leads x {} samples ({:.1} s)",
        rec.leads.len(),
        rec.samples_per_lead(),
        rec.duration_secs()
    );

    // 2. Wrap in a SOP instance + Part-10 file.
    let mut meta = EcgInstanceMeta::new(PersonName::parse("Doe^Jane"), "E123456");
    meta.accession = Some("A789".into());
    meta.content_date = Some("20261011".into());
    meta.content_time = Some("120000".into());
    let ds = recording_to_dataset(&rec, &meta).map_err(|e| e.to_string())?;
    let ts = TransferSyntax::ExplicitVrLittleEndian;
    let file_meta = FileMeta::new(
        ds.string(tags::SOP_CLASS_UID).unwrap(),
        ds.string(tags::SOP_INSTANCE_UID).unwrap(),
        ts,
    );
    let bytes = vyna_dcm::write_part10(&file_meta, &ds).map_err(|e| e.to_string())?;
    let path = std::env::temp_dir().join("vynadcm-ecg-demo.dcm");
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    println!("wrote {} ({} bytes)", path.display(), bytes.len());

    // 3. Read back and verify sample identity + physical conversion.
    let (_, back) = vyna_dcm::read_part10(&bytes).map_err(|e| e.to_string())?;
    let item = &back.sequence(tags::WAVEFORM_SEQUENCE).unwrap()[0];
    let rec2 = recording_from_waveform_item(item).map_err(|e| e.to_string())?;
    assert_eq!(rec2.leads.len(), 12);
    assert_eq!(rec2.leads[1].samples, rec.leads[1].samples);
    let phys = rec2.leads[1].physical_samples();
    println!(
        "lead II sample[0]: raw={} physical={:.3} mV",
        rec2.leads[1].samples[0], phys[0]
    );
    println!("round-trip OK");
    Ok(())
}
