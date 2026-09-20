//! open-bar CLI(初版)。再生エンジンの下調べ用: `probe` / `plan` / `decode`。
//! 音声デバイスへの実出力は次の段階(README参照)。

use open_bar::{combo, media, output, pcm, plan, playlist};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "使い方 / usage:\n  open-bar probe <file>                       ファイル情報(JSON)\n  open-bar plan <file> [--dsd none|dop|native] [--max-rate HZ] [--mqa-dac]\n                                              出力先の能力から再生計画(JSON)\n  open-bar decode <file> <out.wav>            デコードして24bit WAVへ(DSDは自動でPCM化)\n  open-bar play <file> [--volume 0.0-1.0] [--seconds N]   既定の出力デバイスで再生(共有モード。DSDは自動でPCM化)
  open-bar play <file> --exclusive [--bits 16|24] [--volume V]   WASAPI排他モード(Windows、ビットパーフェクト)
  open-bar play <file.dsf> --dop [--seconds N]                   DoPで送る(**DoP対応DACのみ**。非対応だと大音量のノイズ)
  open-bar queue [--rg] [--volume V] <file>...                   複数曲をギャップレスで連続再生(--rg=ReplayGain)
  open-bar pair <folder>                      同名の映像+音声を自動で組み合わせて表示"
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
        Some("play") if args.len() >= 2 => {
            let mut opts = output::PlayOpts::default();
            let (mut exclusive, mut dop, mut bits) = (false, false, 24u16);
            let mut i = 2;
            while i < args.len() {
                if i + 1 >= args.len() && !matches!(args[i].as_str(), "--exclusive" | "--dop") {
                    return usage();
                }
                match args[i].as_str() {
                    "--volume" => opts.volume = args[i + 1].parse().unwrap_or(1.0),
                    "--seconds" => opts.max_seconds = args[i + 1].parse().ok(),
                    "--bits" => bits = args[i + 1].parse().unwrap_or(24),
                    "--exclusive" => {
                        exclusive = true;
                        i += 1;
                        continue;
                    }
                    "--dop" => {
                        dop = true;
                        i += 1;
                        continue;
                    }
                    _ => return usage(),
                }
                i += 2;
            }
            #[cfg(windows)]
            if dop {
                let mut s = match open_mqa_dsd::read_dsd_file(&args[1]) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                if let Some(sec) = opts.max_seconds {
                    let keep = (s.rate_hz as f64 / 8.0 * sec) as usize;
                    s.channels.iter_mut().for_each(|c| c.truncate(keep));
                }
                let (frames, rate) = match open_mqa_dsd::to_dop(&s) {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                return match open_bar::exclusive::play_dop(&frames, rate) {
                    Ok(r) => {
                        println!("DoP再生: {} / {}Hz {}bitコンテナ {}ch / {}フレーム / {:.2}秒", r.device, r.rate_hz, r.container_bits, r.channels, r.frames_written, r.elapsed_secs);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                };
            }
            let info = media::probe(&args[1]);
            let pcm_data = if info.kind == media::MediaKind::Dsd {
                let s = match open_mqa_dsd::read_dsd_file(&args[1]) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                // 先頭からN秒だけ再生する指定なら、変換する前にDSDを切り詰めて時間を節約する
                let mut s = s;
                if let Some(sec) = opts.max_seconds {
                    let keep = (s.rate_hz as f64 / 8.0 * sec) as usize;
                    s.channels.iter_mut().for_each(|c| c.truncate(keep));
                }
                // 共有モードはDoPを通せないので、PCM化(機器が受けやすい176.4kHz以下)して再生する
                let rate = plan::dsd_pcm_candidates(s.rate_hz).into_iter().find(|r| *r <= 176_400).unwrap_or(44_100);
                let cfg = open_mqa_dsd::DsdToPcm { out_rate_hz: rate, cutoff_hz: 40_000.0 };
                match open_mqa_dsd::dsd_to_pcm(&s, cfg) {
                    Ok((v, r)) => pcm::Pcm { sample_rate: r, channels: s.channels.len(), samples: v.iter().map(|x| x * 0.5).collect(), bits_per_sample: None },
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                match pcm::decode_file(&args[1]) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            };
            #[cfg(windows)]
            if exclusive {
                let mut p = pcm_data;
                if let Some(sec) = opts.max_seconds {
                    p.samples.truncate(((sec * p.sample_rate as f64) as usize * p.channels).min(p.samples.len()));
                }
                return match open_bar::exclusive::play_exclusive_pcm(&p.samples, p.channels, p.sample_rate, bits, opts.volume) {
                    Ok(r) => {
                        println!("排他モードで再生: {} / {}Hz {}bitコンテナ {}ch / {}フレーム / {:.2}秒", r.device, r.rate_hz, r.container_bits, r.channels, r.frames_written, r.elapsed_secs);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                };
            }
            match output::play_blocking(&pcm_data, &opts) {
                Ok(r) => {
                    println!("再生しました: {} / デバイス {}Hz {}ch / 変換={} / {}フレーム / {:.2}秒", args[1], r.device_rate_hz, r.device_channels, r.resampled, r.frames_consumed, r.elapsed_secs);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("queue") if args.len() >= 2 => {
            // ギャップレス再生: open-bar queue [--rg] [--volume V] <file>...
            let (mut rg, mut volume) = (false, 1.0f32);
            let mut files = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--rg" => rg = true,
                    "--volume" if i + 1 < args.len() => {
                        volume = args[i + 1].parse().unwrap_or(1.0);
                        i += 1;
                    }
                    f => files.push(f.to_string()),
                }
                i += 1;
            }
            match playlist::load_gapless(&files, rg) {
                Ok(p) => match output::play_blocking(&p, &output::PlayOpts { volume, max_seconds: None }) {
                    Ok(r) => {
                        println!("{}曲をギャップレス再生: {:.1}秒 / デバイス{}Hz / ReplayGain={}", files.len(), r.elapsed_secs, r.device_rate_hz, rg);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                },
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
