//! Generate the wire types from the schema in `nervous_system/proto`.
//!
//! There is one definition of every message Groow sends or stores, and it is not in this
//! language. Rust and Python are both generated from it, so a field that exists on one side
//! exists on the other, and the encoding is canonical protobuf JSON, which both produce
//! identically.
//!
//! JSON rather than the binary encoding, on purpose: the same shapes are what sit in the
//! conversation on disk, and a person should be able to read that with `cat`.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../nervous_system/proto");
    let files = ["wire.proto", "turn.proto", "brain.proto", "records.proto"];
    for f in files {
        println!("cargo:rerun-if-changed={}", root.join(f).display());
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("no OUT_DIR"));
    let descriptor = out.join("descriptors.bin");

    let mut cfg = prost_build::Config::new();
    cfg.file_descriptor_set_path(&descriptor);
    // The well-known types come from pbjson-types, which carries the JSON mapping with them.
    cfg.compile_well_known_types();
    cfg.extern_path(".google.protobuf", "::pbjson_types");
    let paths: Vec<PathBuf> = files.iter().map(|f| root.join(f)).collect();
    cfg.compile_protos(&paths, &[&root]).expect("the schema did not compile");

    let bytes = std::fs::read(&descriptor).expect("no descriptor set");
    pbjson_build::Builder::new()
        .register_descriptors(&bytes)
        .expect("descriptors")
        // Field names as written, so the JSON on the wire reads like the schema rather than
        // being quietly renamed to another convention.
        .preserve_proto_field_names()
        // A record written by a newer version must still be readable by an older one. Without
        // this, one added field makes every old reader refuse the file, which is the opposite
        // of what a schema is for.
        .ignore_unknown_fields()
        .build(&[".groow"])
        .expect("the JSON mapping did not build");
}
