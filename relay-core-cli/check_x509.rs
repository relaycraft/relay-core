use x509_parser::prelude::*;
fn main() {
    let data: &[u8] = &[];
    if let Ok((_, pem)) = x509_parser::pem::parse_x509_pem(data) {
        if let Ok(x509) = pem.parse_x509() {
            for rdn in x509.subject().iter() {
                for attr in rdn.iter() {
                    let _oid = attr.attr_type();
                    let _val = attr.attr_value();
                }
            }
        }
    }
}
