//! Separately cached display metadata; never changes frozen episode selections.
use super::{Cache, Manifest, Result, catalog, fail, transport::Transport};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Display {
    pub policy: String,
    pub show_id: u64,
    pub imdb_id: Option<u32>,
    pub original_title: Option<String>,
    pub year: u16,
}
pub(super) fn valid_override(name: Option<&str>) -> Result<()> {
    if name.is_some_and(|s| !catalog::clean(s, 200)) {
        return Err(fail(
            "display name must be nonempty, control-free UTF-8 (up to 200 bytes)",
        ));
    }
    Ok(())
}
impl Display {
    pub fn validate(&self) -> Result<()> {
        if self.policy != "provider-series-display-v1"
            || self.show_id == 0
            || self.show_id > i32::MAX as u64
            || !(1000..=9999).contains(&self.year)
            || self.imdb_id.is_some_and(|n| n == 0 || n > i32::MAX as u32)
            || self
                .original_title
                .as_ref()
                .is_some_and(|s| !catalog::clean(s, 200))
        {
            return Err(fail("invalid series display metadata"));
        }
        Ok(())
    }
    pub fn matches(&self, m: &Manifest) -> Result<()> {
        self.validate()?;
        if self.show_id != m.show_id
            || m.scope
                .imdb
                .as_ref()
                .is_some_and(|id| crate::imdb_number(id).ok() != self.imdb_id)
        {
            return Err(fail("series display metadata identity mismatch"));
        }
        Ok(())
    }
    pub fn prefix(&self, name: Option<&str>) -> Result<String> {
        self.validate()?;
        valid_override(name)?;
        let title = name.or(self.original_title.as_deref()).ok_or_else(|| {
            fail("provider original title unavailable; supply --show as a display override")
        })?;
        Ok(format!("{title} ({})", self.year))
    }
}
pub(super) fn fetch<T: Transport>(online: &mut T, m: &Manifest) -> Result<Display> {
    let value = online.api(
        &format!("features?feature_id={}&type=tvshow", m.show_id),
        None,
    )?;
    if value
        .get("total_pages")
        .is_some_and(|v| catalog::counter(v).ok() != Some(1))
    {
        return Err(fail("series display metadata pagination incomplete"));
    }
    let rows = value["data"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| fail("series display metadata is not unique"))?;
    if catalog::feature_id(&rows[0])? != m.show_id {
        return Err(fail("series display provider identity mismatch"));
    }
    let a = &rows[0]["attributes"];
    let kind = a["feature_type"]
        .as_str()
        .or_else(|| a["type"].as_str())
        .or_else(|| rows[0]["type"].as_str())
        .unwrap_or("");
    if !kind.eq_ignore_ascii_case("tvshow") && !kind.eq_ignore_ascii_case("tv show") {
        return Err(fail("series display metadata is not a TV show"));
    }
    let original_title =
        if a["original_title"].is_null() || a["original_title"].as_str() == Some("") {
            None
        } else {
            Some(catalog::text(&a["original_title"], 200)?)
        };
    let year = year(&a["year"])?;
    let imdb_id = if a["imdb_id"].is_null() {
        None
    } else {
        Some(catalog::id(&a["imdb_id"])? as u32)
    };
    let result = Display {
        policy: "provider-series-display-v1".into(),
        show_id: m.show_id,
        imdb_id,
        original_title,
        year,
    };
    result.matches(m)?;
    Ok(result)
}
pub(super) fn ensure<T: Transport>(cache: &Cache, m: &Manifest, online: &mut T) -> Result<Display> {
    if let Some(d) = cache.display(m)? {
        return Ok(d);
    }
    println!("Looking up series original title and year (metadata only; no subtitle download).");
    let display = fetch(online, m)?;
    cache.store_display(m, &display)?;
    Ok(display)
}

pub(super) fn year(value: &serde_json::Value) -> Result<u16> {
    match value {
        serde_json::Value::String(s) if s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse::<u16>().ok()
        }
        v => v.as_u64().and_then(|n| u16::try_from(n).ok()),
    }
    .filter(|n| (1000..=9999).contains(n))
    .ok_or_else(|| fail("provider show year missing/invalid; refusing to guess a filename year"))
}
