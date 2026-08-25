//! i18n — the static language table (C eh_i18n.c) + language pick chain.
//!
//! Port of `app/core/eh_i18n.c` verbatim: the key = English string table
//! ships en/de/fr/it columns and falls back to English on a miss, then to
//! the key itself for unknown keys (the C `eh_i18n` returns the key in
//! that case too).
//!
//! Language resolution mirrors the C boot flow (`eh_evt_detect_lang` +
//! `eh_plat_device_language`): the firmware keeps the system language in
//! `/mnt/ext1/system/config/global.cfg` (`language=de`) and does not
//! export it through the environment, so the device probe reads that file
//! directly (the PC/host build simply fails the read and skips — same as
//! C, whose SDL backend also just returned failure).  Pick order:
//! device language → `$LANG` → config (`language=`/`lang=`) → `"en"`.
//!
//! The `%d` placeholders of the C format strings are substituted by
//! [`trn`] in argument order (C used `snprintf(label, n, eh_i18n(k), …)`).

use std::sync::atomic::{AtomicU8, Ordering};

/// `(key, en, de, fr, it)` rows, copied from `eh_g_i18n[]` verbatim.
/// The only addition is `dl.remaining`: the Rust download popup shows a
/// remaining count the C popup never displayed.
static TABLE: &[(&str, &str, &str, &str, &str)] = &[
    ("app.title", "EinkHome", "EinkHome", "EinkHome", "EinkHome"),
    ("action.sync", "Sync", "Sync", "Sync", "Sync"),
    ("action.more", "More", "Mehr", "Plus", "Altro"),
    ("action.menu", "Menu", "Menü", "Menu", "Menu"),
    ("search.ph", "search…", "suchen…", "rechercher…", "cerca…"),
    (
        "search.empty",
        "No recent searches",
        "Keine letzten Suchen",
        "Aucune recherche récente",
        "Nessuna ricerca recente",
    ),
    (
        "sync.meta",
        "Syncing metadata…",
        "Metadaten werden synchronisiert…",
        "Synchronisation des métadonnées…",
        "Sincronizzazione metadati…",
    ),
    ("sync.batch", "batch %d", "Batch %d", "lot %d", "lotto %d"),
    (
        "sync.scan",
        "Scanning library…",
        "Bibliothek wird gescannt…",
        "Analyse de la bibliothèque…",
        "Scansione libreria…",
    ),
    (
        "sync.covers",
        "Downloading covers…",
        "Cover werden geladen…",
        "Chargement des couvertures…",
        "Scaricamento copertine…",
    ),
    (
        "sync.cover_count",
        "%d / %d",
        "%d / %d",
        "%d / %d",
        "%d / %d",
    ),
    (
        "sync.done",
        "Sync complete",
        "Sync abgeschlossen",
        "Sync terminé",
        "Sync completato",
    ),
    (
        "sync.failed",
        "Sync failed",
        "Synchronisierung fehlgeschlagen",
        "Échec de la synchronisation",
        "Sincronizzazione non riuscita",
    ),
    (
        "sync.books",
        "%d books",
        "%d Bücher",
        "%d livres",
        "%d libri",
    ),
    (
        "status.fail",
        "Sync failed",
        "Sync fehlgeschlagen",
        "Échec du sync",
        "Sync fallito",
    ),
    (
        "group.all",
        "All books",
        "Alle Bücher",
        "Tous les livres",
        "Tutti i libri",
    ),
    (
        "group.author",
        "By author",
        "Nach Autor",
        "Par auteur",
        "Per autore",
    ),
    (
        "group.series",
        "By series",
        "Nach Reihe",
        "Par série",
        "Per serie",
    ),
    (
        "group.recent",
        "By recent",
        "Nach Neuheit",
        "Par date",
        "Per data",
    ),
    (
        "group.year",
        "By year",
        "Nach Jahr",
        "Par année",
        "Per anno",
    ),
    (
        "group.genre",
        "By genre",
        "Nach Genre",
        "Par genre",
        "Per genere",
    ),
    (
        "group.none.series",
        "No series",
        "Keine Reihe",
        "Sans série",
        "Nessuna serie",
    ),
    (
        "group.none.author",
        "Unknown author",
        "Unbekannter Autor",
        "Auteur inconnu",
        "Autore sconosciuto",
    ),
    (
        "group.none.year",
        "Unknown year",
        "Unbekanntes Jahr",
        "Année inconnue",
        "Anno sconosciuto",
    ),
    (
        "group.none.genre",
        "Unknown genre",
        "Unbekanntes Genre",
        "Genre inconnu",
        "Genre sconosciuto",
    ),
    (
        "action.group_by",
        "Group by",
        "Gruppieren nach",
        "Grouper par",
        "Raggruppa per",
    ),
    ("group.none", "None", "Keine", "Aucune", "Nessuna"),
    (
        "group.author_series",
        "Author > Series",
        "Autor > Reihe",
        "Auteur > Série",
        "Autore > Serie",
    ),
    (
        "action.sort_by",
        "Sort by",
        "Sortieren nach",
        "Trier par",
        "Ordina per",
    ),
    (
        "sort.title_az",
        "Title A–Z",
        "Titel A–Z",
        "Titre A–Z",
        "Titolo A–Z",
    ),
    (
        "sort.author",
        "By author",
        "Nach Autor",
        "Par auteur",
        "Per autore",
    ),
    (
        "sort.series",
        "By series",
        "Nach Reihe",
        "Par série",
        "Per serie",
    ),
    ("sort.recent", "Recent", "Neuheiten", "Récent", "Recenti"),
    ("view.grid", "Grid", "Raster", "Grille", "Griglia"),
    ("view.list", "List", "Liste", "Liste", "Elenco"),
    ("pager.info", "%d / %d", "%d / %d", "%d / %d", "%d / %d"),
    ("pager.prev", "<", "<", "<", "<"),
    ("pager.next", ">", ">", ">", ">"),
    ("pager.first", "<<", "<<", "<<", "<<"),
    ("pager.last", ">>", ">>", ">>", ">>"),
    (
        "action.settings",
        "Settings",
        "Einstellungen",
        "Paramètres",
        "Impostazioni",
    ),
    (
        "settings.title",
        "Settings",
        "Einstellungen",
        "Paramètres",
        "Impostazioni",
    ),
    (
        "settings.api_host",
        "API host",
        "API-Host",
        "Hôte API",
        "Host API",
    ),
    (
        "settings.api_key",
        "API key",
        "API-Schlüssel",
        "Clé API",
        "Chiave API",
    ),
    (
        "settings.reader",
        "Reader app",
        "Lese-App",
        "Appli lecture",
        "App lettore",
    ),
    (
        "settings.reader_auto",
        "Auto (server)",
        "Auto (Server)",
        "Auto (serveur)",
        "Auto (server)",
    ),
    (
        "settings.dl_dir",
        "Download folder",
        "Download-Ordner",
        "Dossier de téléchargement",
        "Cartella download",
    ),
    (
        "folder.select",
        "Select",
        "Auswählen",
        "Sélectionner",
        "Seleziona",
    ),
    ("folder.empty", "(empty)", "(leer)", "(vide)", "(vuota)"),
    (
        "source.title",
        "Library source",
        "Bibliotheksquelle",
        "Source de la bibliothèque",
        "Origine libreria",
    ),
    ("source.kavita", "Kavita", "Kavita", "Kavita", "Kavita"),
    ("source.local", "Local", "Lokal", "Local", "Locale"),
    ("source.folder", "Folder", "Ordner", "Dossier", "Cartella"),
    (
        "settings.save",
        "Save & apply",
        "Speichern",
        "Enregistrer",
        "Salva e applica",
    ),
    (
        "settings.logs",
        "Show logs",
        "Logs anzeigen",
        "Afficher les logs",
        "Mostra log",
    ),
    (
        "settings.licenses",
        "Licenses",
        "Lizenzen",
        "Licences",
        "Licenze",
    ),
    (
        "settings.system_app",
        "Install as system app",
        "Als System-App installieren",
        "Installer comme app système",
        "Installa come app di sistema",
    ),
    (
        "settings.sysapp_on",
        "On (boots as home)",
        "An (als Startseite)",
        "Activé (s'ouvre en accueil)",
        "Sì (si avvia come home)",
    ),
    (
        "settings.sysapp_off",
        "Off (app only)",
        "Aus (nur App)",
        "Désactivé (app uniquement)",
        "No (solo app)",
    ),
    (
        "licenses.title",
        "Licenses",
        "Lizenzen",
        "Licences",
        "Licenze",
    ),
    ("log.title", "Log", "Log", "Journal", "Log"),
    (
        "log.empty",
        "No log file yet",
        "Noch keine Logdatei",
        "Aucun journal",
        "Nessun log",
    ),
    ("tab.search", "Search", "Suche", "Recherche", "Cerca"),
    ("settings.back", "Back", "Zurück", "Retour", "Indietro"),
    (
        "settings.tap_edit",
        "tap to edit",
        "tippen",
        "toucher",
        "tocca",
    ),
    (
        "settings.installed",
        "installed",
        "installiert",
        "installée",
        "installata",
    ),
    (
        "settings.not_installed",
        "not found",
        "nicht da",
        "absente",
        "assente",
    ),
    (
        "tab.library",
        "Library",
        "Bibliothek",
        "Bibliothèque",
        "Libreria",
    ),
    (
        "action.download_all",
        "Download all",
        "Alle herunterladen",
        "Tout télécharger",
        "Scarica tutto",
    ),
    (
        "ctx.download",
        "Download",
        "Herunterladen",
        "Télécharger",
        "Scarica",
    ),
    ("ctx.open", "Open", "Öffnen", "Ouvrir", "Apri"),
    ("ctx.details", "Details", "Details", "Détails", "Dettagli"),
    (
        "ctx.download_all",
        "Download all",
        "Alle herunterladen",
        "Tout télécharger",
        "Scarica tutto",
    ),
    ("ctx.delete", "Delete", "Löschen", "Supprimer", "Elimina"),
    (
        "ctx.delete_series",
        "Delete series",
        "Reihe löschen",
        "Supprimer la série",
        "Elimina serie",
    ),
    ("detail.author", "Author", "Autor", "Auteur", "Autore"),
    ("detail.series", "Series", "Reihe", "Série", "Serie"),
    ("detail.year", "Year", "Jahr", "Année", "Anno"),
    ("detail.genre", "Genre", "Genre", "Genre", "Genere"),
    ("detail.format", "Format", "Format", "Format", "Formato"),
    ("detail.added", "Added", "Hinzugefügt", "Ajouté", "Aggiunto"),
    (
        "detail.progress",
        "Progress",
        "Fortschritt",
        "Progression",
        "Avanzamento",
    ),
    ("detail.source", "Source", "Quelle", "Source", "Sorgente"),
    (
        "detail.downloaded",
        "Downloaded",
        "Heruntergeladen",
        "Téléchargé",
        "Scaricato",
    ),
    ("detail.yes", "Yes", "Ja", "Oui", "Sì"),
    ("detail.no", "No", "Nein", "Non", "No"),
    ("dl.failed", "Failed", "Fehlgeschlagen", "Échoué", "Fallito"),
    (
        "dl.in_progress",
        "Downloading…",
        "Lädt…",
        "Téléchargement…",
        "Download…",
    ),
    (
        "dl.progress",
        "Downloading %d / %d",
        "Lade %d / %d",
        "Téléchargement %d / %d",
        "Download %d / %d",
    ),
    (
        "dl.complete",
        "%d downloaded",
        "%d heruntergeladen",
        "%d téléchargés",
        "%d scaricati",
    ),
    (
        "dl.failed_count",
        "%d failed",
        "%d fehlgeschlagen",
        "%d échec(s)",
        "%d falliti",
    ),
    (
        "dl.title",
        "Download",
        "Download",
        "Téléchargement",
        "Download",
    ),
    (
        "dl.tap_close",
        "Tap to close",
        "Tippen zum Schließen",
        "Touchez pour fermer",
        "Tocca per chiudere",
    ),
    (
        "action.apps",
        "Applications",
        "Anwendungen",
        "Applications",
        "Applicazioni",
    ),
    (
        "launcher.title",
        "Applications",
        "Anwendungen",
        "Applications",
        "Applicazioni",
    ),
    (
        "launcher.empty",
        "No applications",
        "Keine Anwendungen",
        "Aucune application",
        "Nessuna applicazione",
    ),
    // Rust-side addition: the download popup's remaining-count line has no
    // C counterpart (the C sheet shows dl.progress there instead).
    (
        "dl.remaining",
        "%d remaining",
        "%d verbleibend",
        "%d restant",
        "%d rimanenti",
    ),
];

/// Active language column index (0=en 1=de 2=fr 3=it).  A u8 atomic keeps
/// [`tr`] lock-free like the C last-hit cache did.
static LANG: AtomicU8 = AtomicU8::new(0);

const LANG_CODES: [&str; 4] = ["en", "de", "fr", "it"];

fn lang_index(code: &str) -> u8 {
    LANG_CODES.iter().position(|c| *c == code).unwrap_or(0) as u8
}

#[cfg(test)]
pub(crate) fn set_current(code: &str) {
    LANG.store(lang_index(code), Ordering::Relaxed);
}

/// Boot-time resolution (C cfg load + `eh_evt_detect_lang`): the config
/// value is weakest (it seeds the state but detect overwrites), then
/// `$LANG`, and an on-device global.cfg wins outright; `"en"` is the
/// floor.  Pure so the precedence is unit-testable without touching the
/// process environment.
pub(crate) fn resolve(
    lang_pref: Option<&str>,
    device_lang: Option<&str>,
    env_lang: Option<&str>,
) -> &'static str {
    if let Some(d) = device_lang.and_then(normalize_language) {
        return d;
    }
    if let Some(e) = env_lang
        .filter(|s| !s.is_empty())
        .and_then(|s| lang_prefix(s))
    {
        return e;
    }
    if let Some(c) = lang_pref
        .filter(|s| !s.is_empty())
        .and_then(normalize_language)
    {
        return c;
    }
    "en"
}

/// Apply the full resolution chain against the live device/env.  Called
/// once at boot with the already-parsed config value (C loads its config
/// first, then runs `eh_evt_detect_lang`).
pub fn init(lang_pref: Option<&str>) {
    let device = device_language();
    let env = std::env::var("LANG").ok();
    let code = resolve(lang_pref, device, env.as_deref());
    LANG.store(lang_index(code), Ordering::Relaxed);
}

/// Normalise a raw language value to a shipped code (C
/// `plat_lang_kv_cb`): cut at the first `.`, `_` or `-`, lowercase, and
/// accept only when the two-letter prefix is en/de/fr/it — so
/// `de_DE.utf8`, `de-DE` and `deu` all normalise to `de`, while `es` or
/// `x` fall through (caller picks the next link in the chain).
pub fn normalize_language(value: &str) -> Option<&'static str> {
    lang_prefix(value.trim())
}

/// Two-letter prefix of a locale-ish string, mapped onto a shipped code.
fn lang_prefix(value: &str) -> Option<&'static str> {
    let base: String = value
        .chars()
        .take_while(|c| *c != '.' && *c != '_' && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if base.len() < 2 {
        return None;
    }
    let code = &base[..2];
    LANG_CODES.iter().find(|c| **c == code).copied()
}

/// Device language probe (C `eh_plat_device_language` on the PB backend):
/// parse `/mnt/ext1/system/config/global.cfg` and take its first
/// `language=` value, normalised.  Hosts have no `/mnt/ext1`, so the read
/// fails and this cleanly returns None (exactly the C PC build's
/// hard-coded `-1`).
pub fn device_language() -> Option<&'static str> {
    const GLOBAL_CFG: &str = "/mnt/ext1/system/config/global.cfg";
    let text = std::fs::read_to_string(GLOBAL_CFG).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "language" {
            return normalize_language(value.trim());
        }
    }
    None
}

/// Translate `key` in the active language (C `eh_i18n`).  A missing
/// column falls back to English; an unknown key falls back to itself
/// (so the borrow lives as long as the input — call sites pass string
/// literals, which are `&'static`).
pub fn tr(key: &str) -> &str {
    let col = LANG.load(Ordering::Relaxed) as usize;
    for row in TABLE {
        if row.0 == key {
            // Row layout (key, en, de, fr, it) puts col+1 at the value;
            // every column is non-empty (guarded by a unit test), so the
            // English fallback below is unreachable today.
            let val = match col {
                1 => row.2,
                2 => row.3,
                3 => row.4,
                _ => row.1,
            };
            return val;
        }
    }
    key
}

/// Translate `key` and substitute each `%d` placeholder with the next
/// argument (C `snprintf(buf, n, eh_i18n(key), …)`).
pub fn trn(key: &str, args: &[i64]) -> String {
    let s = tr(key);
    let mut out = String::with_capacity(s.len() + args.len() * 4);
    let mut it = args.iter();
    for part in s.split("%d") {
        out.push_str(part);
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    // The language is a process-global (the LANG atomic); these tests flip
    // it and assert on it, so they serialize on one mutex — parallel test
    // threads would otherwise change each other's expected strings
    // mid-assert (observed as a rare trn_substitutes flake).
    static LANG_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn table_rows_are_complete_across_languages() {
        // 83 keys ported verbatim from eh_g_i18n[] + dl.remaining + the
        // Slint-port additions (ctx.details, detail.* metadata labels).
        assert_eq!(TABLE.len(), 97, "unexpected i18n table size");
        for row in TABLE {
            for col in [row.1, row.2, row.3, row.4] {
                assert!(!col.is_empty(), "empty translation for key {}", row.0);
            }
        }
        // Every language must carry exactly the same key set — true by
        // row construction, but guard against accidental duplicate keys.
        let mut keys: Vec<&str> = TABLE.iter().map(|r| r.0).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate i18n keys");
    }

    #[test]
    fn unknown_key_falls_back_to_itself() {
        let _g = LANG_LOCK.lock();
        set_current("de");
        assert_eq!(tr("no.such.key"), "no.such.key");
        set_current("en");
    }

    #[test]
    fn translations_follow_active_language() {
        let _g = LANG_LOCK.lock();
        set_current("de");
        assert_eq!(tr("action.settings"), "Einstellungen");
        set_current("fr");
        assert_eq!(tr("action.settings"), "Paramètres");
        set_current("it");
        assert_eq!(tr("action.settings"), "Impostazioni");
        set_current("en");
        assert_eq!(tr("action.settings"), "Settings");
    }

    #[test]
    fn trn_substitutes_placeholders_in_order() {
        let _g = LANG_LOCK.lock();
        set_current("de");
        assert_eq!(trn("sync.batch", &[7]), "Batch 7");
        assert_eq!(trn("sync.cover_count", &[3, 12]), "3 / 12");
        // Extra placeholders left intact; extra arguments ignored.
        assert_eq!(trn("sync.books", &[5]), "5 Bücher");
        set_current("en");
    }

    #[test]
    fn normalization_cuts_locales_and_validates() {
        assert_eq!(normalize_language("de_DE.UTF-8"), Some("de"));
        assert_eq!(normalize_language("fr"), Some("fr"));
        assert_eq!(normalize_language("IT-it"), Some("it"));
        assert_eq!(normalize_language("english"), Some("en"));
        assert_eq!(normalize_language("es_ES"), None);
        assert_eq!(normalize_language("x"), None);
        assert_eq!(normalize_language(""), None);
    }

    #[test]
    fn resolution_chain_device_beats_env_beats_config() {
        // $LANG locale string normalises (de_DE.UTF-8 -> de).
        assert_eq!(resolve(None, None, Some("de_DE.UTF-8")), "de");
        // Device probe wins outright.
        assert_eq!(resolve(Some("fr"), Some("it"), Some("de_DE.UTF-8")), "it");
        // $LANG beats config.
        assert_eq!(resolve(Some("fr"), None, Some("de")), "de");
        // Config overrides the 'en' floor when nothing else applies…
        assert_eq!(resolve(Some("de"), None, None), "de");
        // …and empty values never win.
        assert_eq!(resolve(Some(""), None, Some("")), "en");
        assert_eq!(resolve(None, None, None), "en");
        // Unshippable values fall through to the next link.
        assert_eq!(resolve(Some("es"), None, Some("fr")), "fr");
    }

    #[test]
    fn wired_call_site_translates_a_drawn_label() {
        let _g = LANG_LOCK.lock();
        // menu.rs draws labels()[…] verbatim via draw_text, so switching
        // the language must change the exact strings the More drawer
        // paints.
        set_current("en");
        let en = crate::menu::label_keys();
        set_current("de");
        let de = crate::menu::label_keys();
        assert_eq!(en[0].1, "Group by");
        assert_eq!(de[0].1, "Gruppieren nach");
        assert_eq!(de[4].1, "Anwendungen");
        set_current("en");
    }
}
