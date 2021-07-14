use super::font::Font;
use super::itemize::FamilyKind;
use fount::{FontContext, FontData, FontId, Library, Locale, SourceId};
use std::collections::HashMap;
use swash::proxy::CharmapProxy;
use swash::text::cluster::*;
use swash::text::Script;
use swash::{Attributes, FontRef};

// Make this configurable?
const RETAINED_SOURCE_COUNT: usize = 12;

#[derive(Clone)]
pub struct FontCache {
    pub context: FontContext,
    sources: SourceCache,
    selected_params: Option<(FamilyKind, Attributes)>,
    selected_fonts: Vec<CachedFont>,
    fallback_params: (Script, Option<Locale>, Attributes),
    fallback_fonts: Vec<CachedFont>,
    emoji_font: Option<CachedFont>,
}

impl FontCache {
    pub fn new(library: &Library) -> Self {
        Self {
            context: FontContext::new(library),
            sources: SourceCache::default(),
            selected_params: None,
            selected_fonts: vec![],
            fallback_params: (Script::Unknown, None, Attributes::default()),
            fallback_fonts: vec![],
            emoji_font: None,
        }
    }

    pub fn reset(&mut self) {
        self.selected_params = None;
        self.selected_fonts.clear();
        self.fallback_params = (Script::Unknown, None, Attributes::default());
        self.fallback_fonts.clear();
        self.sources.serial += 1;
        self.sources.prune();
    }

    pub fn select_family(&mut self, kind: FamilyKind, attrs: Attributes) {
        if self.selected_params != Some((kind, attrs)) {
            self.selected_params = Some((kind, attrs));
            let families = match &kind {
                FamilyKind::Named(id) => core::slice::from_ref(id),
                FamilyKind::Default => self.context.default_families(),
                FamilyKind::Generic(family) => self.context.generic_families(*family),
            };
            self.selected_fonts.clear();
            let context = &self.context;
            self.selected_fonts.extend(
                families
                    .iter()
                    .filter_map(|id| context.family(*id))
                    .filter_map(|family| family.query(attrs))
                    .map(CachedFont::new),
            );
        }
    }

    pub fn select_fallbacks(&mut self, script: Script, locale: Option<Locale>, attrs: Attributes) {
        if self.fallback_params != (script, locale, attrs) {
            self.fallback_params = (script, locale, attrs);
            self.fallback_fonts.clear();
            let context = &self.context;
            let fallback_families = context.fallback_families(script, locale);
            self.fallback_fonts.extend(
                fallback_families
                    .iter()
                    .filter_map(|id| context.family(*id))
                    .filter_map(|family| family.query(attrs))
                    .map(CachedFont::new),
            );
        }
    }

    pub fn map_cluster(&mut self, cluster: &mut CharCluster) -> Option<Font> {
        let mut best = None;
        if map_cluster(
            &self.context,
            &mut self.sources,
            &mut self.selected_fonts,
            cluster,
            &mut best,
        ) {
            return best;
        }
        if cluster.info().is_emoji() {
            if self.emoji_font.is_none() {
                use fount::GenericFamily;
                self.emoji_font = self
                    .context
                    .generic_families(GenericFamily::Emoji)
                    .iter()
                    .filter_map(|id| self.context.family(*id))
                    .filter_map(|family| family.query(Attributes::default()))
                    .map(CachedFont::new)
                    .next()
            }
            if let Some(emoji_font) = &mut self.emoji_font {
                if map_cluster(
                    &self.context,
                    &mut self.sources,
                    core::slice::from_mut(emoji_font),
                    cluster,
                    &mut best,
                ) {
                    return best;
                }
            }
        }
        map_cluster(
            &self.context,
            &mut self.sources,
            &mut self.fallback_fonts,
            cluster,
            &mut best,
        );
        best
    }
}

fn map_cluster(
    context: &FontContext,
    sources: &mut SourceCache,
    fonts: &mut [CachedFont],
    cluster: &mut CharCluster,
    best: &mut Option<Font>,
) -> bool {
    for font in fonts {
        if font.map_cluster(context, sources, cluster, best) {
            return true;
        }
    }
    false
}

#[derive(Clone, Default)]
struct SourceCache {
    sources: HashMap<SourceId, (u64, FontData)>,
    serial: u64,
}

impl SourceCache {
    fn prune(&mut self) {
        let mut target_serial = self.serial.saturating_sub(RETAINED_SOURCE_COUNT as u64);
        let mut count = self.sources.len();
        while count > RETAINED_SOURCE_COUNT {
            self.sources.retain(|_, v| {
                if count <= RETAINED_SOURCE_COUNT && v.0 <= target_serial {
                    count -= 1;
                    false
                } else {
                    true
                }
            });
            target_serial += 1;
        }
    }

    fn get(&mut self, context: &FontContext, id: FontId) -> Option<Font> {
        let entry = context.font(id)?;
        let source_id = entry.source();
        let data = if let Some(cached_source) = self.sources.get_mut(&source_id) {
            cached_source.0 = self.serial;
            cached_source.1.clone()
        } else {
            let data = context.load(source_id)?;
            self.sources.insert(source_id, (self.serial, data.clone()));
            data
        };
        let font_ref = FontRef::from_index(&data, entry.index() as usize)?;
        let offset = font_ref.offset;
        Some(Font {
            data,
            offset,
            key: entry.cache_key(),
        })
    }
}

#[derive(Clone)]
struct CachedFont {
    id: FontId,
    font: Option<(Font, CharmapProxy)>,
    error: bool,
}

impl CachedFont {
    fn new(id: FontId) -> Self {
        Self {
            id,
            font: None,
            error: false,
        }
    }

    fn map_cluster(
        &mut self,
        context: &FontContext,
        sources: &mut SourceCache,
        cluster: &mut CharCluster,
        best: &mut Option<Font>,
    ) -> bool {
        if self.error {
            return false;
        }
        let (font, charmap_proxy) = if let Some(font) = &self.font {
            (&font.0, font.1)
        } else {
            if let Some(font) = sources.get(context, self.id) {
                self.font = Some((font.clone(), CharmapProxy::from_font(&font.as_ref())));
                let (font, charmap_proxy) = self.font.as_ref().unwrap();
                (font, *charmap_proxy)
            } else {
                self.error = true;
                return false;
            }
        };
        let charmap = charmap_proxy.materialize(&font.as_ref());
        match cluster.map(|ch| charmap.map(ch)) {
            Status::Complete => {
                *best = Some(font.clone());
                return true;
            }
            Status::Keep => {
                *best = Some(font.clone());
            }
            Status::Discard => {}
        }
        false
    }
}
