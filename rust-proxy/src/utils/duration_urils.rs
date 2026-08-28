pub mod human_duration {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn parse_duration_str(s: &str) -> Result<Duration, String> {
        let s = s.trim();
        if let Some(num_str) = s.strip_suffix("ms") {
            num_str
                .parse::<u64>()
                .map(Duration::from_millis)
                .map_err(|e| e.to_string())
        } else if let Some(num_str) = s.strip_suffix('s') {
            num_str
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|e| e.to_string())
        } else if let Some(num_str) = s.strip_suffix('m') {
            num_str
                .parse::<u64>()
                .map(|m| Duration::from_secs(m * 60))
                .map_err(|e| e.to_string())
        } else if let Some(num_str) = s.strip_suffix('h') {
            num_str
                .parse::<u64>()
                .map(|h| Duration::from_secs(h * 3600))
                .map_err(|e| e.to_string())
        } else if let Some(num_str) = s.strip_suffix('d') {
            num_str
                .parse::<u64>()
                .map(|d| Duration::from_secs(d * 86400))
                .map_err(|e| e.to_string())
        } else {
            s.parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|_| format!("invalid duration format: '{s}'"))
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_duration_str(&s).map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}s", duration.as_secs_f64());
        serializer.serialize_str(&s)
    }
}
#[cfg(test)]
mod tests {

    use crate::utils::duration_urils::human_duration::parse_duration_str;

    use serde::{Deserialize, Serialize};
    use std::time::Duration;
    #[test]
    fn test_parse_seconds() {
        assert_eq!(parse_duration_str("10s"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration_str("0s"), Ok(Duration::from_secs(0)));
    }

    #[test]
    fn test_parse_milliseconds() {
        assert_eq!(parse_duration_str("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration_str("1ms"), Ok(Duration::from_millis(1)));
    }

    #[test]
    fn test_parse_minutes() {
        assert_eq!(parse_duration_str("1m"), Ok(Duration::from_secs(60)));
        assert_eq!(parse_duration_str("2m"), Ok(Duration::from_secs(120)));
    }
    #[test]
    fn test_parse_hours() {
        assert_eq!(parse_duration_str("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration_str("2h"), Ok(Duration::from_secs(7200)));
    }
    #[test]
    fn test_parse_days() {
        assert_eq!(parse_duration_str("1d"), Ok(Duration::from_secs(86400)));
        assert_eq!(parse_duration_str("7d"), Ok(Duration::from_secs(604800)));
    }

    #[test]
    fn test_parse_default_as_seconds() {
        assert_eq!(parse_duration_str("120"), Ok(Duration::from_secs(120)));
    }

    #[test]
    fn test_parse_with_whitespace() {
        assert_eq!(parse_duration_str("  5s  "), Ok(Duration::from_secs(5)));
        assert_eq!(parse_duration_str(" 2m "), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration_str(" 3h "), Ok(Duration::from_secs(10800)));
    }

    #[test]
    fn test_parse_invalid_format() {
        assert!(parse_duration_str("10w").is_err());
        assert!(parse_duration_str("abc").is_err());
        assert!(parse_duration_str("10 s").is_err());
        assert!(parse_duration_str("ms").is_err());
        assert!(parse_duration_str("").is_err());
    }

    #[test]
    fn test_parse_invalid_number() {
        assert!(parse_duration_str("abcs").is_err());
        assert!(parse_duration_str("1..2s").is_err());
        assert!(parse_duration_str("5.m").is_err());
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct TestStruct {
        #[serde(with = "super::human_duration")]
        timeout: Duration,
    }

    #[test]
    fn test_serialization() {
        let test_data = TestStruct {
            timeout: Duration::from_secs(15),
        };
        let json = serde_json::to_string(&test_data).unwrap();
        assert!(json == r#"{"timeout":"15s"}"# || json == r#"{"timeout":"15.0s"}"#);

        let test_data_f64 = TestStruct {
            timeout: Duration::from_secs_f64(0.5),
        };
        let json_f64 = serde_json::to_string(&test_data_f64).unwrap();
        assert_eq!(json_f64, r#"{"timeout":"0.5s"}"#);
    }

    #[test]
    fn test_deserialization() {
        let json_s = r#"{"timeout":"30s"}"#;
        let expected = TestStruct {
            timeout: Duration::from_secs(30),
        };
        assert_eq!(
            serde_json::from_str::<TestStruct>(json_s).unwrap(),
            expected
        );
        let json_ms = r#"{"timeout":"750ms"}"#;
        let expected = TestStruct {
            timeout: Duration::from_millis(750),
        };
        assert_eq!(
            serde_json::from_str::<TestStruct>(json_ms).unwrap(),
            expected
        );

        let json_m = r#"{"timeout":"90s"}"#;
        let expected = TestStruct {
            timeout: Duration::from_secs(90),
        };
        assert_eq!(
            serde_json::from_str::<TestStruct>(json_m).unwrap(),
            expected
        );

        let json_default = r#"{"timeout":"5"}"#;
        let expected = TestStruct {
            timeout: Duration::from_secs(5),
        };
        assert_eq!(
            serde_json::from_str::<TestStruct>(json_default).unwrap(),
            expected
        );
    }

    #[test]
    fn test_deserialization_error() {
        let json = r#"{"timeout":"2w"}"#;
        let result = serde_json::from_str::<TestStruct>(json);
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("invalid duration format: '2w'"));
    }

    #[test]
    fn test_round_trip() {
        let original = TestStruct {
            timeout: Duration::from_millis(2500),
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized = serde_json::from_str::<TestStruct>(&json)
            .unwrap_err()
            .to_string();

        assert!(deserialized.contains("invalid digit found in string"));
    }
}
