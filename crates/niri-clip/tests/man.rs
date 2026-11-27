use std::process::Command;

#[test]
fn man_page_lists_invocations_without_subpage_references() {
    let output = Command::new(env!("CARGO_BIN_EXE_niri-clip"))
        .arg("man")
        .output()
        .unwrap();
    assert!(output.status.success());
    let page = String::from_utf8(output.stdout).unwrap();

    assert!(page.contains(".SH SUBCOMMANDS\n"));
    assert!(page.contains(r"\fBniri\-clip search\fR"));
    assert!(page.contains(r"\fBniri\-clip install\-service\fR"));
    assert!(page.contains(r"niri\-clip <子命令> \-\-help"));
    assert!(!page
        .lines()
        .any(|line| line.starts_with(r"niri\-clip\-") && line.ends_with("(1)")));
}
