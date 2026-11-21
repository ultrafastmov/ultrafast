//! The canonical text of a JSON value: object keys sorted by their UTF-8
//! bytes, no whitespace, strings escaped and numbers written as serde_json
//! writes them (integers in decimal, other numbers as the shortest text that
//! reads back as the same double, e.g. `0.1`, `1.0`, `1e+21`). Parsing the
//! canonical text and writing it again gives the same bytes, so a document
//! hashed by one party (the composer) and re-read by another (the render
//! service) hashes the same, whatever language moves the bytes in between,
//! as long as it does not re-serialize them.

use serde_json::Value;

/// `v` as canonical JSON text.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("a string serializes"));
                out.push(':');
                write(&m[k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("a scalar serializes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_compact_and_a_fixed_point() {
        let text = r#"{ "b": [1.0, 1e21, 0.1, -0.0, 1E-7, 12345678901234567890, 3.141592653589793],
                        "a": {"z": "é\u0001/\"", "y": null, "x": true} }"#;
        let v: Value = serde_json::from_str(text).unwrap();
        let c = canonical_json(&v);
        assert_eq!(
            c,
            r#"{"a":{"x":true,"y":null,"z":"é\u0001/\""},"b":[1.0,1e+21,0.1,-0.0,1e-7,12345678901234567890,3.141592653589793]}"#
        );
        let again: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(canonical_json(&again), c);
    }

    /// Doubles must read back exactly (serde_json's `float_roundtrip`):
    /// without it these two came back one unit in the last place off, and
    /// a composer's canonical text hashed differently after the service
    /// re-read it.
    #[test]
    fn doubles_read_back_exactly() {
        for text in ["1381.0559388932143", "907.7331595499569"] {
            let v: Value = serde_json::from_str(text).unwrap();
            assert_eq!(canonical_json(&v), text);
        }
        // And any double, written the canonical way, reads back the same.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let d = f64::from_bits(x);
            if !d.is_finite() {
                continue;
            }
            let text = canonical_json(&Value::from(d));
            let back: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(back.as_f64().map(f64::to_bits), Some(d.to_bits()), "{text}");
            assert_eq!(canonical_json(&back), text);
        }
    }

    /// `ULTRAFAST_CANONICAL_REPRO=doc.json cargo test -p ultrafast-graph -- --ignored`:
    /// a canonical document's bytes survive a read and a canonical write.
    #[test]
    #[ignore]
    fn repro_file_is_a_fixed_point() {
        let path = std::env::var("ULTRAFAST_CANONICAL_REPRO").expect("ULTRAFAST_CANONICAL_REPRO");
        let text = std::fs::read_to_string(path).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(canonical_json(&v) == text, "the canonical text changed on re-reading");
    }
}
