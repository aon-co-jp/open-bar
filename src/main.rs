//! open-bar CLI(初版)。再生エンジンの下調べ用: `probe` / `plan` / `decode`。
//! 音声デバイスへの実出力は次の段階(README参照)。

use open_bar::{combo, media, pcm, plan};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "使い方 / usage:\n  open-bar probe <file>                       ファイル情報(JSON)\n  open-bar plan <file> [--dsd none|dop|native] [--max-rate HZ] [--mqa-dac]\n                                              出力先の能力から再生計画(JSON)\n  open-bar decode <file> <out.wav>            デコードして24bit WAVへ(DSDは自動でPCM化)\n  open-bar pair <folder>                      同名の映像+音声を自動で組み合わせて表示"
    );
    ExitCode::from(2)
}

fn source_kind(info: &media::MediaInfo) -> Option<plan::SourceKind> {
    match info.kind {
        media::MediaKind::Dsd => Some(plan::SourceKind::Dsd { rate_hz: info.sample_rate_hz? }),
        _ if info.is_mqa => Some(plan::SourceKind::Mqa { rate_hz: info.sample_rate_hz?, bits: info.bits_per_sample.unwrap_or(24) }),
        _ => Some(plan::SourceKind::Pcm { rate_hz: info.sample_rate_hz?, bits: info.bits_per_sample.unwrap_or(16) }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") if args.len() == 2 => {
            println!("{}", serde_json::to_string_pretty(&media::probe(&args[1])).unwrap());
            ExitCode::SUCCESS
        }
        Some("plan") if args.len() >= 2 => {
            let info = media::probe(&args[1]);
            let Some(src) = source_kind(&info) else {
                eprintln!("再生できる音声情報が読み取れません: {}", args[1]);
                return ExitCode::FAILURE;
            };
            let mut caps = plan::DeviceCaps { max_pcm_rate_hz: 192_000, max_bits: 24, exclusive_mode: true, dsd: plan::DsdSupport::None, mqa_decoder: false };
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--dsd" if i + 1 < args.len() => {
                        caps.dsd = match args[i + 1].as_str() {
                            "native" => plan::DsdSupport::Native,
                            "dop" => plan::DsdSupport::Dop,
                            _ => plan::DsdSupport::None,
                        };
                        i += 1;
                    }
                    "--max-rate" if i + 1 < args.len() => {
                        caps.max_pcm_rate_hz = args[i + 1].parse().unwrap_or(caps.max_pcm_rate_hz);
                        i += 1;
                    }
                    "--mqa-dac" => caps.mqa_decoder = true,
                    _ => return usage(),
                }
                i += 1;
            }
            println!("{}", serde_json::to_string_pretty(&plan::plan(src, caps, plan::PlayOptions { bit_perfect: true })).unwrap());
            ExitCode::SUCCESS
        }
        Some("decode") if args.len() == 3 => {
            let info = media::probe(&args[1]);
            let (samples, rate, channels) = if info.kind == media::MediaKind::Dsd {
                let s = match open_mqa_dsd::read_dsd_file(&args[1]) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                match open_mqa_dsd::dsd_to_pcm(&s, open_mqa_dsd::DsdToPcm::default_for(s.rate_hz)) {
                    Ok((v, r)) => (v, r, s.channels.len()),
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                match pcm::decode_file(&args[1]) {
                    Ok(p) => (p.samples, p.sample_rate, p.channels),
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            };
            let per: Vec<Vec<i32>> = (0..channels).map(|c| samples.iter().skip(c).step_by(channels).map(|v| (v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).collect()).collect();
            match open_mqa::wav::encode_wav(&per, rate, 24) {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&args[2], bytes) {
                        eprintln!("書き込めません: {e}");
                        return ExitCode::FAILURE;
                    }
                    println!("{} → {} ({rate}Hz, {channels}ch, 24bit)", args[1], args[2]);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("pair") if args.len() == 2 => {
            for c in combo::auto_pair(&args[1]) {
                println!("{}", c.to_json());
            }
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
