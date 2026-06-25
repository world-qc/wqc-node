//! Egress GeoIP resolution (no device GPS). Used for orchestrator bid telemetry only.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

pub const GEO_CACHE_TTL_SECS: i64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeoInfo {
    pub latitude: f64,
    pub longitude: f64,
    pub country: String,
    pub city: String,
}

#[derive(Deserialize)]
struct IpApiJsonResponse {
    lat: f64,
    lon: f64,
    country: String,
    city: String,
    status: String,
}

#[derive(Deserialize)]
struct FreeIpApiResponse {
    latitude: f64,
    longitude: f64,
    #[serde(rename = "countryName")]
    country_name: String,
    #[serde(rename = "cityName")]
    city_name: String,
}

#[derive(Deserialize)]
struct IpApiCoResponse {
    latitude: f64,
    longitude: f64,
    country_name: String,
    city: String,
}

/// Multi-provider fallback for outages and HTTP 429 rate limits.
pub async fn fetch_geolocation(client: &Client) -> Option<GeoInfo> {
    if let Some(info) = try_ip_api(client).await {
        return Some(info);
    }
    tracing::warn!("GeoIP: ip-api.com failed or rate-limited; trying freeipapi.com");

    if let Some(info) = try_free_ip_api(client).await {
        return Some(info);
    }
    tracing::warn!("GeoIP: freeipapi.com failed or rate-limited; trying ipapi.co");

    if let Some(info) = try_ipapi_co(client).await {
        return Some(info);
    }

    tracing::error!("GeoIP: all provider fallbacks exhausted");
    None
}

async fn try_ip_api(client: &Client) -> Option<GeoInfo> {
    let res = client
        .get("http://ip-api.com/json/?fields=status,country,city,lat,lon")
        .send()
        .await
        .ok()?;
    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    if !res.status().is_success() {
        return None;
    }
    let json = res.json::<IpApiJsonResponse>().await.ok()?;
    if json.status != "success" {
        return None;
    }
    Some(GeoInfo {
        latitude: json.lat,
        longitude: json.lon,
        country: json.country,
        city: json.city,
    })
}

async fn try_free_ip_api(client: &Client) -> Option<GeoInfo> {
    let res = client
        .get("https://freeipapi.com/api/json")
        .send()
        .await
        .ok()?;
    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || !res.status().is_success() {
        return None;
    }
    let json = res.json::<FreeIpApiResponse>().await.ok()?;
    Some(GeoInfo {
        latitude: json.latitude,
        longitude: json.longitude,
        country: json.country_name,
        city: json.city_name,
    })
}

async fn try_ipapi_co(client: &Client) -> Option<GeoInfo> {
    let res = client.get("https://ipapi.co/json/").send().await.ok()?;
    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || !res.status().is_success() {
        return None;
    }
    let json = res.json::<IpApiCoResponse>().await.ok()?;
    Some(GeoInfo {
        latitude: json.latitude,
        longitude: json.longitude,
        country: json.country_name,
        city: json.city,
    })
}

pub fn build_geo_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("wqc-node/0.1 geoip")
        .build()
        .unwrap_or_else(|_| Client::new())
}
