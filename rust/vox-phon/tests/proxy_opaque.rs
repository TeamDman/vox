//! Runtime opaque payload errors must remain recoverable at the public API.
use facet::Facet;
use vox_types::Payload;

#[derive(Debug, Facet)]
#[facet(proxy = String)]
struct Checked(String);
impl TryFrom<&Checked> for String {
    type Error = &'static str;
    fn try_from(v: &Checked) -> Result<Self, Self::Error> {
        if v.0 == "reject" {
            Err("rejected")
        } else {
            Ok(v.0.clone())
        }
    }
}
impl From<String> for Checked {
    fn from(v: String) -> Self {
        Self(v)
    }
}

#[derive(Facet)]
#[facet(proxy = Vec<u8>)]
struct Unsupported(Vec<u8>);
impl From<&Unsupported> for Vec<u8> {
    fn from(v: &Unsupported) -> Self {
        v.0.clone()
    }
}
impl From<Vec<u8>> for Unsupported {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

// r[verify descriptors.explicit-proxy]
#[test]
fn opaque_proxy_errors_are_recoverable() {
    const CHILD: &str = "TEAMY_PHON_OPAQUE_REGRESSION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // A regression to the old extern-C panic aborts this child, rather than
        // obscuring the remaining test results in the parent test process.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "opaque_proxy_errors_are_recoverable",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "opaque child failed: {:?}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let rejected = Checked("reject".into());
    let payload = Payload::outgoing(&rejected);
    assert!(vox_phon::to_vec(&payload).is_err());
    let nested = Payload::outgoing(&payload);
    assert!(vox_phon::to_vec(&nested).is_err());
    assert_eq!(rejected.0, "reject"); // outgoing data remains borrowed
    let unsupported = Unsupported(vec![1, 2]);
    assert!(vox_phon::to_vec(&Payload::outgoing(&unsupported)).is_err());
    let good = Checked("accepted".into());
    let bytes = vox_phon::to_vec(&Payload::outgoing(&good)).unwrap();
    let back = vox_phon::from_slice_borrowed::<Payload<'_>>(&bytes).unwrap();
    let Payload::Encoded(inner) = back else {
        panic!("expected borrowed payload")
    };
    assert_eq!(
        vox_phon::from_slice::<Checked>(inner).unwrap().0,
        "accepted"
    );
}
