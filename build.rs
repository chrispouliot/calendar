use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=data/ui/window.blp");
    println!("cargo::rerun-if-changed=data/ui/common/date-chooser.blp");
    println!("cargo::rerun-if-changed=data/ui/common/quick-add-popover.blp");
    println!("cargo::rerun-if-changed=data/ui/common/event-popover.blp");
    println!("cargo::rerun-if-changed=data/ui/views/month-view.blp");
    println!("cargo::rerun-if-changed=data/ui/views/week-view.blp");
    println!("cargo::rerun-if-changed=data/ui/views/agenda-view.blp");
    println!("cargo::rerun-if-changed=data/style.css");
    println!("cargo::rerun-if-changed=data/resources.gresource.xml");
    println!("cargo::rerun-if-changed=data/icons/scalable/emblems/calendar-month-symbolic.svg");
    println!("cargo::rerun-if-changed=data/icons/scalable/emblems/calendar-week-symbolic.svg");
    println!("cargo::rerun-if-changed=data/icons/scalable/emblems/calendar-agenda-symbolic.svg");
    println!("cargo::rerun-if-changed=data/icons/scalable/emblems/calendar-today-symbolic.svg");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ui_out_dir = out_dir.join("ui");

    std::fs::create_dir_all(ui_out_dir.join("views")).unwrap();
    std::fs::create_dir_all(ui_out_dir.join("common")).unwrap();

    // Compile Blueprint files to .ui in OUT_DIR/ui/
    let status = std::process::Command::new("blueprint-compiler")
        .args([
            "batch-compile",
            ui_out_dir.to_str().unwrap(),
            "data/ui",
            "data/ui/window.blp",
            "data/ui/common/date-chooser.blp",
            "data/ui/common/quick-add-popover.blp",
            "data/ui/common/event-popover.blp",
            "data/ui/views/month-view.blp",
            "data/ui/views/week-view.blp",
            "data/ui/views/agenda-view.blp",
        ])
        .status()
        .expect("Failed to run blueprint-compiler");
    assert!(status.success(), "blueprint-compiler batch-compile failed");

    // Compile GResource manifest into OUT_DIR/calendar.gresource
    // Use both data/ and OUT_DIR/ as source directories so the XML can
    // reference the generated .ui files.
    let status = std::process::Command::new("glib-compile-resources")
        .args([
            "--sourcedir=data",
            "--sourcedir",
            out_dir.to_str().unwrap(),
            "--target",
            &out_dir.join("calendar.gresource").to_string_lossy(),
            "data/resources.gresource.xml",
        ])
        .status()
        .expect("Failed to run glib-compile-resources");
    assert!(status.success(), "glib-compile-resources failed");
}
