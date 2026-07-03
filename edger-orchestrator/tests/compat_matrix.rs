use std::path::Path;

fn compat_matrix() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../planning/edger/docs/compat-matrix.md");
    std::fs::read_to_string(path).expect("compat matrix")
}

fn assert_row_status(matrix: &str, row: &str, status: &str) {
    let line = matrix
        .lines()
        .find(|line| line.contains(row))
        .unwrap_or_else(|| panic!("missing compat row: {row}"));
    assert!(
        line.contains(&format!("| {status} |")),
        "row {row} should be {status}: {line}"
    );
}

#[test]
fn must_preserve_rows_are_published_as_tested() {
    let matrix = compat_matrix();
    for row in [
        "Worker addressing",
        "Runtime orchestration boundary",
        "Entrypoint autodiscovery priority",
        "`fetch(req) -> Response` contract",
        "Wasm standalone worker",
        "Static SPA",
        "ApiKeyPrincipal + namespaces",
        "Reserved paths",
        "Ingress body/header limits",
        "Request-id/log correlation",
        "Cron internal requests",
    ] {
        assert_row_status(&matrix, row, "tested");
    }
}

#[test]
fn known_partial_rows_remain_explicit() {
    let matrix = compat_matrix();
    for row in ["Manifest fields", "CommonJS Node server examples"] {
        assert_row_status(&matrix, row, "partial");
    }
}
