use orbit_core::SourceReference;
use serde_json::Value;

pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

pub fn print_sources(sources: &[SourceReference]) {
    if sources.is_empty() {
        return;
    }
    println!("\nSources:");
    for source in sources {
        println!("- {source}");
    }
}
