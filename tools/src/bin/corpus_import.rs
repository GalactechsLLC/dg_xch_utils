use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;
use std::path::PathBuf;

// Match the replay harnesses (they load frames with ChiaProtocolVersion::default()).
const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut blobs = None;
    let mut out = None;
    let mut start = None;
    let mut end = None;
    let mut anchor: Option<u32> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut take = || args.next().ok_or(format!("missing value for {a}"));
        match a.as_str() {
            "--blobs" => blobs = Some(PathBuf::from(take()?)),
            "--out" => out = Some(PathBuf::from(take()?)),
            "--start" => start = Some(take()?.parse::<u32>()?),
            "--end" => end = Some(take()?.parse::<u32>()?),
            "--anchor" => anchor = Some(take()?.parse::<u32>()?),
            other => return Err(format!("unknown arg {other}").into()),
        }
    }
    let (blobs, out) = (
        blobs.ok_or("--blobs required")?,
        out.ok_or("--out required")?,
    );
    let (start, end) = (
        start.ok_or("--start required")?,
        end.ok_or("--end required")?,
    );
    std::fs::create_dir_all(&out)?;

    // Out-of-window generator back-references: blocks may point at generators tens of thousands of
    // heights below the window (block-level generator compression). Collect them so they can be
    // exported and packed as `ref_block_<h>.bin` frames the replay harness overlays.
    let mut out_of_window_refs = std::collections::BTreeSet::new();
    let mut window_start = start;
    while window_start <= end {
        let window_end = window_start.saturating_add(31).min(end);
        let mut frame = Vec::with_capacity(32);
        for h in window_start..=window_end {
            let raw = std::fs::read(blobs.join(h.to_string()))
                .map_err(|e| format!("blob for height {h}: {e}"))?;
            let bytes = maybe_unzstd(raw)?;
            let block = FullBlock::from_bytes(&mut Cursor::new(&bytes[..]), VERSION)
                .map_err(|e| format!("parse FullBlock at {h}: {e}"))?;
            if block.reward_chain_block.height != h {
                return Err(format!(
                    "height mismatch: blob {h} decodes to block {}",
                    block.reward_chain_block.height
                )
                .into());
            }
            for r in &block.transactions_generator_ref_list {
                if *r < start || *r > end {
                    out_of_window_refs.insert(*r);
                }
            }
            frame.push(block);
        }
        let msg = RespondBlocks {
            start_height: window_start,
            end_height: window_end,
            blocks: frame,
        };
        let path = out.join(format!("blocks_{window_start}_{window_end}.bin"));
        std::fs::write(&path, msg.to_bytes(VERSION)?)?;
        println!("wrote {}", path.display());
        window_start = window_end.saturating_add(1);
    }
    let mut missing_refs = Vec::new();
    let mut packed_refs = 0usize;
    for r in &out_of_window_refs {
        let block_path = blobs.join(r.to_string());
        let record_path = blobs.join(format!("record_{r}"));
        if block_path.exists() && record_path.exists() {
            let bytes = maybe_unzstd(std::fs::read(&block_path)?)?;
            let block = FullBlock::from_bytes(&mut Cursor::new(&bytes[..]), VERSION)
                .map_err(|e| format!("parse ref FullBlock at {r}: {e}"))?;
            let msg = RespondBlocks {
                start_height: *r,
                end_height: *r,
                blocks: vec![block],
            };
            std::fs::write(
                out.join(format!("ref_block_{r}.bin")),
                msg.to_bytes(VERSION)?,
            )?;
            let rec_bytes = maybe_unzstd(std::fs::read(&record_path)?)?;
            let record = parse_chia_db_record(&rec_bytes)
                .map_err(|e| format!("parse ref record at {r}: {e}"))?;
            std::fs::write(
                out.join(format!("ref_record_{r}.bin")),
                record.to_bytes(VERSION)?,
            )?;
            packed_refs += 1;
        } else {
            missing_refs.push(r.to_string());
        }
    }
    println!(
        "refs: {} out-of-window, {packed_refs} packed",
        out_of_window_refs.len()
    );
    if !missing_refs.is_empty() {
        println!("MISSING_REFS {}", missing_refs.join(","));
    }

    if let Some(h) = anchor {
        let mut run = Vec::new();
        let mut cursor = h;
        loop {
            let path = blobs.join(format!("record_{cursor}"));
            if !path.exists() {
                break;
            }
            let bytes = maybe_unzstd(std::fs::read(&path)?)?;
            let record = parse_chia_db_record(&bytes)
                .map_err(|e| format!("parse chia DB BlockRecord at {cursor}: {e}"))?;
            if record.height != cursor {
                return Err(format!("anchor record height {} != {cursor}", record.height).into());
            }
            run.push(record);
            if cursor == 0 {
                break;
            }
            cursor -= 1;
        }
        run.reverse(); // ascending
        let path = out.join(format!("anchor_records_{h}.bin"));
        std::fs::write(&path, run.to_bytes(VERSION)?)?;
        println!("wrote {} ({} records)", path.display(), run.len());
    }
    Ok(())
}

// The reference DB's BlockRecord layout is byte-identical to ours (the VDF outputs are bare
// fixed-100-byte ClassgroupElements), so decode directly with exact-fit framing: from_bytes and
// no trailing bytes.
fn parse_chia_db_record(bytes: &[u8]) -> Result<BlockRecord, Box<dyn std::error::Error>> {
    let mut c = Cursor::new(bytes);
    let record = BlockRecord::from_bytes(&mut c, VERSION)?;
    if c.position() != bytes.len() as u64 {
        return Err("trailing bytes after imported BlockRecord".into());
    }
    Ok(record)
}

fn maybe_unzstd(raw: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if raw.len() >= 4 && raw[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        Ok(zstd::decode_all(&raw[..])?)
    } else {
        Ok(raw)
    }
}
