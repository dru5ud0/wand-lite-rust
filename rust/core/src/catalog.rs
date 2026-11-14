use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTitle {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub thumbnail: Option<String>,
    pub steam_app_ids: Vec<u64>,
    pub has_trainer: bool,
}

#[derive(Debug)]
pub struct Catalog {
    titles: Vec<CatalogTitle>,
}

#[derive(Debug, Deserialize)]
struct Feed {
    titles: HashMap<String, TitleRecord>,
    games: HashMap<String, GameRecord>,
}

#[derive(Debug, Deserialize)]
struct TitleRecord {
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    thumbnail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GameRecord {
    #[serde(rename = "titleId")]
    title_id: String,
    #[serde(default, rename = "platformId")]
    platform_id: String,
    #[serde(default, rename = "correlationIds")]
    correlation_ids: Vec<String>,
    trainer: Option<serde_json::Value>,
}

impl Catalog {
    pub fn fetch_live() -> Result<Self, String> {
        let bytes = Self::fetch_live_bytes()?;
        Self::parse(&bytes).map_err(|error| format!("catalog format changed: {error}"))
    }

    pub fn fetch_live_bytes() -> Result<Vec<u8>, String> {
        const URL: &str = "https://storage-cdn.wemod.com/catalog.json";
        const MAX_BYTES: usize = 12 * 1024 * 1024;
        let response = minreq::get(URL)
            .with_timeout(12)
            .send_lazy()
            .map_err(|error| format!("catalog request failed: {error}"))?;
        if response.status_code != 200 {
            return Err(format!("catalog returned HTTP {}", response.status_code));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("catalog read failed: {error}"))?;
        if bytes.len() > MAX_BYTES {
            return Err("catalog response is too large".to_owned());
        }
        Ok(bytes)
    }

    pub fn parse(json: &[u8]) -> Result<Self, serde_json::Error> {
        let feed: Feed = serde_json::from_slice(json)?;
        let mut games_by_title: HashMap<String, Vec<GameRecord>> = HashMap::new();
        for game in feed.games.into_values() {
            games_by_title
                .entry(game.title_id.clone())
                .or_default()
                .push(game);
        }
        let mut titles = Vec::with_capacity(feed.titles.len());
        for (id, title) in feed.titles {
            let mut steam_app_ids = Vec::new();
            let mut has_trainer = false;
            if let Some(games) = games_by_title.remove(&id) {
                for game in games {
                    has_trainer |= game.trainer.is_some();
                    if game.platform_id == "steam" {
                        steam_app_ids.extend(game.correlation_ids.iter().filter_map(
                            |correlation| correlation.strip_prefix("steam:")?.parse::<u64>().ok(),
                        ));
                    }
                }
            }
            steam_app_ids.sort_unstable();
            steam_app_ids.dedup();
            titles.push(CatalogTitle {
                id,
                name: title.name,
                slug: title.slug,
                thumbnail: title.thumbnail,
                steam_app_ids,
                has_trainer,
            });
        }
        titles.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(Self { titles })
    }

    pub fn len(&self) -> usize {
        self.titles.len()
    }

    pub fn all(&self) -> &[CatalogTitle] {
        &self.titles
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }

    pub fn find_by_steam_id(&self, app_id: u64) -> Option<&CatalogTitle> {
        self.titles
            .iter()
            .find(|title| title.steam_app_ids.contains(&app_id))
    }

    pub fn find_by_id(&self, id: &str) -> Option<&CatalogTitle> {
        self.titles.iter().find(|title| title.id == id)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&CatalogTitle> {
        let query = query.trim().to_lowercase();
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut prefix = Vec::new();
        let mut substring = Vec::new();
        for title in &self.titles {
            let name = title.name.to_lowercase();
            if name.starts_with(&query) {
                prefix.push(title);
            } else if name.contains(&query) {
                substring.push(title);
            }
        }
        prefix.into_iter().chain(substring).take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_titles_and_steam_games_from_feed() {
        let json = br#"{"titles":{"97315":{"name":"Stick It to the Stickman","slug":"stick-it-to-the-stickman","thumbnail":"/title_thumbnails/97315/test.webp"},"2":{"name":"Other Stickman","slug":"other"}},"games":{"105784":{"titleId":"97315","platformId":"steam","correlationIds":["steam:2085540"],"trainer":{"cheatCount":5}}}}"#;
        let catalog = Catalog::parse(json).unwrap();
        assert_eq!(catalog.len(), 2);
        let title = catalog.find_by_steam_id(2085540).unwrap();
        assert_eq!(title.id, "97315");
        assert!(title.has_trainer);
        assert_eq!(catalog.search("stick", 1)[0].id, "97315");
        assert_eq!(catalog.search("stickman", 2).len(), 2);
    }
}
