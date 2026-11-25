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
}
