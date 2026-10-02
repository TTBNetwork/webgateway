use acme;

pub fn main() {
    acme::CertificateBuilder::default()
        .private_key(include_str!(
            "/develop_workspaces/rust/webgateway/tests/txit_atxa_ttb-network_main_privkey.pem"
        ))
        .build();
}
