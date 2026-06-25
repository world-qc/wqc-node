//! GeoIP cache orchestration (SQLite + external providers).

use reqwest::Client;

use crate::domain::geo::{fetch_geolocation, GeoInfo};
use crate::infra::storage::Storage;

/// Load from 24h SQLite cache, or fetch via provider fallback chain and persist.
pub async fn resolve_node_location(
    storage: &Storage,
    client: &Client,
) -> Option<GeoInfo> {
    match storage.get_cached_geo() {
        Ok(Some(cached)) => {
            tracing::info!(
                "GeoIP: using cached location ({}, {})",
                cached.city,
                cached.country
            );
            return Some(cached);
        }
        Ok(None) => tracing::info!("GeoIP: cache miss or expired; querying external APIs"),
        Err(e) => tracing::error!("GeoIP: SQLite cache read failed: {e}"),
    }

    let fresh = fetch_geolocation(client).await?;
    if let Err(e) = storage.save_geo_cache(&fresh) {
        tracing::error!("GeoIP: SQLite cache write failed: {e}");
    } else {
        tracing::info!(
            "GeoIP: cached location for 24h ({}, {})",
            fresh.city,
            fresh.country
        );
    }
    Some(fresh)
}
