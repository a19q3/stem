fn main() {
    for schema in ["schemas/ledger.mol", "schemas/dictionary.mol"] {
        println!("cargo:rerun-if-changed={schema}");
        molecule_codegen::Compiler::new()
            .input_schema_file(schema)
            .generate_code(molecule_codegen::Language::Rust)
            .output_dir_set_default()
            .run()
            .expect("generate Molecule types");
    }
    println!("cargo:rerun-if-changed=strand/generate.py");
    println!("cargo:rerun-if-changed=schemas/strand.json");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let status = std::process::Command::new("python3")
        .args([
            "strand/generate.py",
            "--schema",
            "schemas/strand.json",
            "--rust-out",
        ])
        .arg(out.join("strand_wire.rs"))
        .arg("--c-out")
        .arg(out.join("strand_wire.h"))
        .status()
        .expect("run the Strand schema compiler with Python 3");
    assert!(status.success(), "generate Strand types");
}
