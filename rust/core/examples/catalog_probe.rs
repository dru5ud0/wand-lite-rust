use std::fs;
use wand_rebuild_core::catalog::Catalog;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("pass a catalog.json path")?;
    let catalog = Catalog::parse(&fs::read(path)?)?;
    let title = catalog
        .find_by_steam_id(2085540)
        .ok_or("Stickman not found")?;
    println!(
        "titles={} stickman_title={} trainer={}",
        catalog.len(),
        title.id,
        title.has_trainer
    );
    Ok(())
}
