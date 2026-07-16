fn main() {
    tonic_build::configure()
        .build_server(false)
        .compile_protos(&["../proto/echo.proto"], &["../proto"])
        .unwrap();
}
