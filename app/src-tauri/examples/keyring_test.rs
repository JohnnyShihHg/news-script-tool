fn main() {
    let entry = keyring::Entry::new("news-script-tool", "gemini_api_key").expect("create entry");
    match entry.set_password("test-value-123") {
        Ok(()) => println!("set_password OK"),
        Err(e) => { println!("set_password FAILED: {e}"); return; }
    }
    match entry.get_password() {
        Ok(v) => println!("get_password OK: {v}"),
        Err(e) => println!("get_password FAILED: {e}"),
    }
    match entry.delete_credential() {
        Ok(()) => println!("delete_credential OK"),
        Err(e) => println!("delete_credential FAILED: {e}"),
    }
}
