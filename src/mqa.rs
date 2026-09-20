//! MQAファイルの判別。**MQAは復号しない**(特許・非公開技術で、本プロジェクトは再実装しない)。
//! MQA対応DACで鳴らす場合、ソフトは音量調整・EQ・SRCなしのビットパーフェクトで素通しし、展開はDAC側に任せる。
//! 判別はFLAC内のタグ(`MQAENCODER` / `ORIGINALSAMPLERATE`)を探す簡易方式(MQA本体の同期ワード検出ではない)。

/// 先頭64KiB内にMQAのエンコーダタグがあるか。
pub fn is_mqa_bytes(head: &[u8]) -> bool {
    let hay = &head[..head.len().min(65_536)];
    let has = |needle: &[u8]| hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    has(b"MQAENCODER") || has(b"ORIGINALSAMPLERATE")
}

pub fn is_mqa_file(path: &str) -> bool {
    use std::io::Read;
    let mut buf = vec![0u8; 65_536];
    match std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => is_mqa_bytes(&buf[..n]),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mqa_tags_case_insensitively_and_ignores_plain_flac() {
        assert!(is_mqa_bytes(b"fLaC....MQAENCODER=MQAEncode v1.1, 2.4.2+0 (b6...)"));
        assert!(is_mqa_bytes(b"xx originalsamplerate=96000 xx"));
        assert!(!is_mqa_bytes(b"fLaC....ENCODER=reference libFLAC 1.3.2"));
        assert!(!is_mqa_bytes(b""));
    }
}
