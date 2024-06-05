use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use lazy_static::lazy_static;
use openssl::asn1::Asn1Time;
use openssl::x509::X509;
use rusty_vault::core::{Core, SealConfig};
use rusty_vault::errors::RvError;
use rusty_vault::logical::{Operation, Request, Response};
use rusty_vault::storage::{barrier_aes_gcm, physical};
use serde_json::{json, Map, Value};

const ROLE: &str = "test";

lazy_static! {
    static ref CA: CAInfo = {
        let dir = PathBuf::from("/tmp/rusty_vault_pki_module");
        // let dir = env::temp_dir().join("rusty_vault_pki_module"); // TODO: 1. 能否复用文件 2. 改成数据库？

        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        assert!(fs::create_dir(&dir).is_ok());
        let mut root_token = String::new();

        let mut conf: HashMap<String, Value> = HashMap::new();
        conf.insert("path".to_string(), Value::String(dir.to_string_lossy().into_owned()));

        let backend = physical::new_backend("file", &conf).unwrap(); // file or database
        let barrier = barrier_aes_gcm::AESGCMBarrier::new(Arc::clone(&backend));

        let c = Arc::new(RwLock::new(Core { physical: backend, barrier: Arc::new(barrier), ..Default::default() }));

        {
            let mut core = c.write().unwrap();
            assert!(core.config(Arc::clone(&c), None).is_ok());

            let seal_config = SealConfig { secret_shares: 10, secret_threshold: 5 };

            let result = core.init(&seal_config);
            assert!(result.is_ok());
            let init_result = result.unwrap();
            println!("init_result: {:?}", init_result);

            let mut unsealed = false;
            for i in 0..seal_config.secret_threshold {
                let key = &init_result.secret_shares[i as usize];
                let unseal = core.unseal(key);
                assert!(unseal.is_ok());
                unsealed = unseal.unwrap();
            }

            root_token = init_result.root_token;
            println!("root_token: {:?}", root_token);

            assert!(unsealed);
        }

        config_ca(Arc::clone(&c), &root_token);
        generate_root(Arc::clone(&c), &root_token, false);

        CAInfo { core: c, token: root_token }
    };
}

struct CAInfo {
    core: Arc<RwLock<Core>>,
    token: String,
}
fn read_api(core: &Core, token: &str, path: &str) -> Result<Option<Response>, RvError> {
    let mut req = Request::new(path);
    req.operation = Operation::Read;
    req.client_token = token.to_string();
    let resp = core.handle_request(&mut req);
    resp
}

fn write_api(
    core: &Core,
    token: &str,
    path: &str,
    data: Option<Map<String, Value>>,
) -> Result<Option<Response>, RvError> {
    let mut req = Request::new(path);
    req.operation = Operation::Write;
    req.client_token = token.to_string();
    req.body = data;

    let resp = core.handle_request(&mut req);
    println!("path: {}, req.body: {:?}", path, req.body);
    resp
}

fn config_ca(core: Arc<RwLock<Core>>, token: &str) {
    let core = core.read().unwrap();

    // mount pki backend to path: pki/
    let mount_data = json!({
        "type": "pki",
    })
    .as_object()
    .unwrap()
    .clone();

    let resp = write_api(&core, token, "sys/mounts/pki/", Some(mount_data));
    assert!(resp.is_ok());
}

/// - `data`: see [RoleEntry](rusty_vault::modules::pki::path_roles)
fn config_role(data: Value) {
    let core = CA.core.read().unwrap();
    let token = &CA.token;

    let role_data = data.as_object()
        .expect("`data` must be a JSON object")
        .clone();

    // config role
    let result = write_api(&core, token, &format!("pki/roles/{}", ROLE), Some(role_data));
    assert!(result.is_ok());
}

/// generate root cert, so that you can read from `pki/ca/pem`
/// - if `exported` is true, then the response will contain `private key`
fn generate_root(core: Arc<RwLock<Core>>, token: &str, exported: bool) {
    let core = core.read().unwrap();

    let key_type = "rsa";
    let key_bits = 4096;
    let common_name = "mega-ca";
    let req_data = json!({
            "common_name": common_name,
            "ttl": "365d",
            "country": "cn",
            "key_type": key_type,
            "key_bits": key_bits,
        })
        .as_object()
        .unwrap()
        .clone();

    let resp = write_api(
        &core,
        token,
        format!("pki/root/generate/{}", if exported { "exported" } else { "internal" }).as_str(),
        Some(req_data),
    );
    assert!(resp.is_ok());
    // let resp_body = resp.unwrap();
    // let data = resp_body.unwrap().data;
    // let key_data = data.unwrap();
    // println!("generate root result: {:?}", key_data);

    // let resp_ca_pem = read_api(&core, token, "pki/ca/pem");
    // let resp_ca_pem_cert_data = resp_ca_pem.unwrap().unwrap().data.unwrap();
    //
    // println!("resp_ca_pem_cert_data: {:?}", resp_ca_pem_cert_data);
}

/// - `data`: see [issue_path](rusty_vault::modules::pki::path_issue)
pub fn issue_cert(data: Value) -> String {
    let core = CA.core.read().unwrap();
    let token = &CA.token;

    // let dns_sans = ["test.com", "a.test.com", "b.test.com"];
    let issue_data = data.as_object()
        .expect("`data` must be a JSON object")
        .clone();

    // issue cert
    let resp = write_api(&core, token, &format!("pki/issue/{}", ROLE), Some(issue_data));
    assert!(resp.is_ok());
    let resp_body = resp.unwrap();
    let cert_data = resp_body.unwrap().data.unwrap();
    println!("issue cert result: {:?}", cert_data["certificate"]);

    #[cfg(test)]
    {
        let mut file = fs::File::create("/tmp/cert.crt").unwrap(); // TODO add root cert in it
        file.write_all(cert_data["certificate"].as_str().unwrap().as_ref()).unwrap();
    }

    cert_data["certificate"].as_str().unwrap().to_owned()
}

pub fn verify_cert(cert_pem: &[u8]) -> bool {
    let ca_cert = {
        let core = CA.core.read().unwrap();

        let resp_ca_pem = read_api(&core, &CA.token, "pki/ca/pem").unwrap().unwrap();
        let ca_cert = resp_ca_pem.data.unwrap();
        let ca_cert_pem = ca_cert["certificate"].as_str().unwrap();
        X509::from_pem(ca_cert_pem.as_ref()).unwrap()
    };

    let cert = X509::from_pem(cert_pem).unwrap();
    // verify time
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let now = Asn1Time::from_unix(now).unwrap();
    let not_before = cert.not_before();
    let not_after = cert.not_after();
    match now.compare(not_before) {
        Ok(Ordering::Less) | Err(_) => return false,
        _ => {}
    }
    match now.compare(not_after) {
        Ok(Ordering::Greater) | Err(_) => return false,
        _ => {}
    }

    // verify signature
    cert.verify(&ca_cert.public_key().unwrap()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pki_issue() {
        config_role(json!({
            "ttl": "60d",
            "max_ttl": "365d",
            "key_type": "rsa",
            "key_bits": 4096,
            "country": "CN",
            "province": "Beijing",
            "locality": "Beijing",
            "organization": "OpenAtom-Mega",
            "no_store": false,
        }));

        let cert_pem = issue_cert(json!({
            "ttl": "10d",
            "common_name": "test.com",
            "alt_names": "a.test.com,b.test.com",
        }));

        assert!(verify_cert(cert_pem.as_ref()));
    }
}

#[cfg(test)]
mod tests_raw {
    use std::{
        collections::HashMap,
        default::Default,
        env, fs,
        sync::{Arc, RwLock},
        time::{SystemTime, UNIX_EPOCH},
    };
    use std::io::Write;

    use go_defer::defer;
    use openssl::{asn1::Asn1Time, ec::EcKey, nid::Nid, pkey::PKey, rsa::Rsa, x509::X509};
    use rusty_vault::{
        core::{Core, SealConfig},
        logical::{Operation, Request},
        storage::{barrier_aes_gcm, physical},
    };
    use rusty_vault::errors::RvError;
    use rusty_vault::logical::Response;
    use serde_json::{json, Map, Value};

    fn test_read_api(core: &Core, token: &str, path: &str, is_ok: bool) -> Result<Option<Response>, RvError> {
        let mut req = Request::new(path);
        req.operation = Operation::Read;
        req.client_token = token.to_string();
        let resp = core.handle_request(&mut req);
        assert_eq!(resp.is_ok(), is_ok);
        resp
    }

    fn test_write_api(
        core: &Core,
        token: &str,
        path: &str,
        is_ok: bool,
        data: Option<Map<String, Value>>,
    ) -> Result<Option<Response>, RvError> {
        let mut req = Request::new(path);
        req.operation = Operation::Write;
        req.client_token = token.to_string();
        req.body = data;

        let resp = core.handle_request(&mut req);
        println!("path: {}, req.body: {:?}", path, req.body);
        assert_eq!(resp.is_ok(), is_ok);
        resp
    }

    fn test_pki_config_ca(core: Arc<RwLock<Core>>, token: &str) {
        let core = core.read().unwrap();

        // mount pki backend to path: pki/
        let mount_data = json!({
            "type": "pki",
        })
            .as_object()
            .unwrap()
            .clone();

        let resp = test_write_api(&core, token, "sys/mounts/pki/", true, Some(mount_data));
        assert!(resp.is_ok());
    }

    fn test_pki_config_role(core: Arc<RwLock<Core>>, token: &str) {
        let core = core.read().unwrap();

        let role_data = json!({
            "ttl": "60d",
            "max_ttl": "365d",
            "key_type": "rsa",
            "key_bits": 4096,
            "country": "CN",
            "province": "Beijing",
            "locality": "Beijing",
            "organization": "OpenAtom",
            "no_store": false,
        })
            .as_object()
            .unwrap()
            .clone();

        // config role
        assert!(test_write_api(&core, token, "pki/roles/test", true, Some(role_data)).is_ok());
        let resp = test_read_api(&core, token, "pki/roles/test", true);
        assert!(resp.as_ref().unwrap().is_some());
        let resp = resp.unwrap();
        assert!(resp.is_some());
        let data = resp.unwrap().data;
        assert!(data.is_some());
        let role_data = data.unwrap();
        println!("role_data: {:?}", role_data);
        assert_eq!(role_data["ttl"].as_u64().unwrap(), 60 * 24 * 60 * 60);
        assert_eq!(role_data["max_ttl"].as_u64().unwrap(), 365 * 24 * 60 * 60);
        assert_eq!(role_data["not_before_duration"].as_u64().unwrap(), 30);
        assert_eq!(role_data["key_type"].as_str().unwrap(), "rsa");
        assert_eq!(role_data["key_bits"].as_u64().unwrap(), 4096);
        assert_eq!(role_data["country"].as_str().unwrap(), "CN");
        assert_eq!(role_data["province"].as_str().unwrap(), "Beijing");
        assert_eq!(role_data["locality"].as_str().unwrap(), "Beijing");
        assert_eq!(role_data["organization"].as_str().unwrap(), "OpenAtom");
        assert_eq!(role_data["no_store"].as_bool().unwrap(), false);
    }

    fn test_pki_generate_root(core: Arc<RwLock<Core>>, token: &str, exported: bool, is_ok: bool) {
        let core = core.read().unwrap();

        let key_type = "rsa";
        let key_bits = 4096;
        let common_name = "test-ca";
        let req_data = json!({
            "common_name": common_name,
            "ttl": "365d",
            "country": "cn",
            "key_type": key_type,
            "key_bits": key_bits,
        })
            .as_object()
            .unwrap()
            .clone();
        // println!("generate root req_data: {:?}, is_ok: {}", req_data, is_ok);
        let resp = test_write_api(
            &core,
            token,
            format!("pki/root/generate/{}", if exported { "exported" } else { "internal" }).as_str(),
            is_ok,
            Some(req_data),
        );
        if !is_ok {
            return;
        }
        let resp_body = resp.unwrap();
        assert!(resp_body.is_some());
        let data = resp_body.unwrap().data;
        assert!(data.is_some());
        let key_data = data.unwrap();
        // println!("generate root result: {:?}", key_data);

        let resp_ca_pem = test_read_api(&core, token, "pki/ca/pem", true);
        let resp_ca_pem_cert_data = resp_ca_pem.unwrap().unwrap().data.unwrap();

        let ca_cert = X509::from_pem(resp_ca_pem_cert_data["certificate"].as_str().unwrap().as_bytes()).unwrap();
        let subject = ca_cert.subject_name();
        let cn = subject.entries_by_nid(Nid::COMMONNAME).next().unwrap();
        assert_eq!(cn.data().as_slice(), common_name.as_bytes());

        let not_after = Asn1Time::days_from_now(365).unwrap();
        let ttl_diff = ca_cert.not_after().diff(&not_after);
        assert!(ttl_diff.is_ok());
        let ttl_diff = ttl_diff.unwrap();
        assert_eq!(ttl_diff.days, 0);

        if exported {
            assert!(key_data["private_key_type"].as_str().is_some());
            assert_eq!(key_data["private_key_type"].as_str().unwrap(), key_type);
            assert!(key_data["private_key"].as_str().is_some());
            let private_key_pem = key_data["private_key"].as_str().unwrap();
            match key_type {
                "rsa" => {
                    let rsa_key = Rsa::private_key_from_pem(private_key_pem.as_bytes());
                    assert!(rsa_key.is_ok());
                    assert_eq!(rsa_key.unwrap().size() * 8, key_bits);
                }
                "ec" => {
                    let ec_key = EcKey::private_key_from_pem(private_key_pem.as_bytes());
                    assert!(ec_key.is_ok());
                    assert_eq!(ec_key.unwrap().group().degree(), key_bits);
                }
                _ => {}
            }
        } else {
            assert!(key_data.get("private_key").is_none());
        }
    }

    fn test_pki_issue_cert_by_generate_root(core: Arc<RwLock<Core>>, token: &str) {
        let core = core.read().unwrap();

        let dns_sans = ["test.com", "a.test.com", "b.test.com"];
        let issue_data = json!({
            "ttl": "10d",
            "common_name": "test.com",
            "alt_names": "a.test.com,b.test.com",
        })
            .as_object()
            .unwrap()
            .clone();

        // issue cert
        let resp = test_write_api(&core, token, "pki/issue/test", true, Some(issue_data));
        assert!(resp.is_ok());
        let resp_body = resp.unwrap();
        assert!(resp_body.is_some());
        let data = resp_body.unwrap().data;
        assert!(data.is_some());
        let cert_data = data.unwrap();
        println!("issue cert result: {:?}", cert_data["certificate"]);

        let mut file = fs::File::create("/tmp/cert.crt").unwrap();
        file.write_all(cert_data["certificate"].as_str().unwrap().as_ref()).unwrap();

        let cert = X509::from_pem(cert_data["certificate"].as_str().unwrap().as_bytes()).unwrap();
        let alt_names = cert.subject_alt_names();
        assert!(alt_names.is_some());
        let alt_names = alt_names.unwrap();
        assert_eq!(alt_names.len(), dns_sans.len());
        for alt_name in alt_names {
            assert!(dns_sans.contains(&alt_name.dnsname().unwrap()));
        }
        assert_eq!(cert_data["private_key_type"].as_str().unwrap(), "rsa");
        let priv_key = PKey::private_key_from_pem(cert_data["private_key"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(priv_key.bits(), 4096);
        assert!(priv_key.public_eq(&cert.public_key().unwrap()));
        let serial_number = cert.serial_number().to_bn().unwrap();
        let serial_number_hex = serial_number.to_hex_str().unwrap();
        assert_eq!(
            cert_data["serial_number"].as_str().unwrap().replace(':', "").to_lowercase().as_str(),
            serial_number_hex.to_lowercase().as_str()
        );
        let expiration_time = Asn1Time::from_unix(cert_data["expiration"].as_i64().unwrap()).unwrap();
        let ttl_compare = cert.not_after().compare(&expiration_time);
        assert!(ttl_compare.is_ok());
        assert_eq!(ttl_compare.unwrap(), std::cmp::Ordering::Equal);
        let now_timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expiration_ttl = cert_data["expiration"].as_u64().unwrap();
        let ttl = expiration_ttl - now_timestamp;
        let expect_ttl = 10 * 24 * 60 * 60;
        assert!(ttl <= expect_ttl);
        assert!((ttl + 10) > expect_ttl);

        let authority_key_id = cert.authority_key_id();
        assert!(authority_key_id.is_some());

        println!("authority_key_id: {}", hex::encode(authority_key_id.unwrap().as_slice()));

        let resp_ca_pem = test_read_api(&core, token, "pki/ca/pem", true);
        let resp_ca_pem_cert_data = resp_ca_pem.unwrap().unwrap().data.unwrap();

        let ca_cert = X509::from_pem(resp_ca_pem_cert_data["certificate"].as_str().unwrap().as_bytes()).unwrap();
        let subject = ca_cert.subject_name();
        let cn = subject.entries_by_nid(Nid::COMMONNAME).next().unwrap();
        assert_eq!(cn.data().as_slice(), "test-ca".as_bytes());
        println!("ca subject_key_id: {}", hex::encode(ca_cert.subject_key_id().unwrap().as_slice()));
        assert_eq!(ca_cert.subject_key_id().unwrap().as_slice(), authority_key_id.unwrap().as_slice());
    }

    #[test]
    fn test_pki_module() {
        let dir = env::temp_dir().join("rusty_vault_pki_module");
        assert!(fs::create_dir(&dir).is_ok());
        defer! (
            assert!(fs::remove_dir_all(&dir).is_ok());
        );

        let mut root_token = String::new();
        println!("root_token: {:?}", root_token);

        let mut conf: HashMap<String, Value> = HashMap::new();
        conf.insert("path".to_string(), Value::String(dir.to_string_lossy().into_owned()));

        let backend = physical::new_backend("file", &conf).unwrap();
        let barrier = barrier_aes_gcm::AESGCMBarrier::new(Arc::clone(&backend));

        let c = Arc::new(RwLock::new(Core { physical: backend, barrier: Arc::new(barrier), ..Default::default() }));

        {
            let mut core = c.write().unwrap();
            assert!(core.config(Arc::clone(&c), None).is_ok());

            let seal_config = SealConfig { secret_shares: 10, secret_threshold: 5 };

            let result = core.init(&seal_config);
            assert!(result.is_ok());
            let init_result = result.unwrap();
            println!("init_result: {:?}", init_result);

            let mut unsealed = false;
            for i in 0..seal_config.secret_threshold {
                let key = &init_result.secret_shares[i as usize];
                let unseal = core.unseal(key);
                assert!(unseal.is_ok());
                unsealed = unseal.unwrap();
            }

            root_token = init_result.root_token;

            assert!(unsealed);
        }

        {
            println!("root_token: {:?}", root_token);
            test_pki_config_ca(Arc::clone(&c), &root_token);
            test_pki_generate_root(Arc::clone(&c), &root_token, false, true);
            test_pki_config_role(Arc::clone(&c), &root_token);
            test_pki_issue_cert_by_generate_root(Arc::clone(&c), &root_token);
        }
    }
}
