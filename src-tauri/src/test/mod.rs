use crate::azure_auth::{access_token_args, validated_cli_identifier, COGNITIVE_SERVICES_RESOURCE};
use crate::commands::fast_transcription_endpoint;
use crate::video::normalize_yuv420_dimensions;

#[test]
fn accepts_only_azure_cognitive_services_custom_domains() {
    let endpoint =
        fast_transcription_endpoint("https://meetly-speech.cognitiveservices.azure.com/").unwrap();
    assert_eq!(
        endpoint.as_str(),
        "https://meetly-speech.cognitiveservices.azure.com/speechtotext/transcriptions:transcribe?api-version=2025-10-15"
    );

    assert!(fast_transcription_endpoint(
        "https://meetly-speech.cognitiveservices.azure.com.evil.example/"
    )
    .is_err());
    assert!(fast_transcription_endpoint(
        "https://evil.example/meetly-speech.cognitiveservices.azure.com"
    )
    .is_err());
    assert!(fast_transcription_endpoint(
        "https://meetly-speech.cognitiveservices.azure.com/redirect"
    )
    .is_err());
}

#[test]
fn normalizes_odd_dimensions_for_yuv420() {
    assert_eq!(
        normalize_yuv420_dimensions(1_714, 1_085).unwrap(),
        (1_714, 1_084)
    );
    assert_eq!(
        normalize_yuv420_dimensions(1_710, 965).unwrap(),
        (1_710, 964)
    );
}

#[test]
fn rejects_dimensions_that_cannot_contain_a_yuv420_frame() {
    assert!(normalize_yuv420_dimensions(1, 20).is_err());
    assert!(normalize_yuv420_dimensions(20, 1).is_err());
}

#[test]
fn accepts_azure_identifiers_without_shell_metacharacters() {
    assert!(validated_cli_identifier(
        Some("11111111-2222-3333-4444-555555555555".to_string()),
        "Tenant ID"
    )
    .is_ok());
    assert!(
        validated_cli_identifier(Some("contoso.onmicrosoft.com".to_string()), "Tenant ID").is_ok()
    );
    assert!(access_token_args(
        COGNITIVE_SERVICES_RESOURCE,
        None,
        Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string())
    )
    .is_ok());
}

#[test]
fn rejects_values_that_cmd_could_interpret() {
    for value in [
        "tenant&calc.exe",
        "tenant|whoami",
        "tenant>output.txt",
        "tenant%PATH%",
        "tenant name",
        "tenant\"name",
    ] {
        assert!(validated_cli_identifier(Some(value.to_string()), "Tenant ID").is_err());
    }
}
