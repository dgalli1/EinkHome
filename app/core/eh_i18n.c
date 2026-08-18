/* eh_i18n.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_i18n.h"

/* ── i18n ────────────────────────────────────────────────────────────── */

char eh_g_lang[8] = "en";

/* Trivial i18n table.  Key = English string; value = translation.
 * Add rows here for new languages.  Falls back to English on miss.
 */

const BsI18n eh_g_i18n[] = {
    {"app.title", "EinkHome", "EinkHome", "EinkHome", "EinkHome"},
    {"action.sync", "Sync", "Sync", "Sync", "Sync"},
    {"action.more", "More", "Mehr", "Plus", "Altro"},
    {"action.menu", "Menu", "Men\u00fc", "Menu", "Menu"},
    {"search.ph", "search\u2026", "suchen\u2026", "rechercher\u2026", "cerca\u2026"},
    {"search.empty",
     "No recent searches",
     "Keine letzten Suchen",
     "Aucune recherche r\u00e9cente",
     "Nessuna ricerca recente"},
    {"sync.meta",
     "Syncing metadata\u2026",
     "Metadaten werden synchronisiert\u2026",
     "Synchronisation des m\u00e9tadonn\u00e9es\u2026",
     "Sincronizzazione metadati\u2026"},
    {"sync.batch", "batch %d", "Batch %d", "lot %d", "lotto %d"},
    {"sync.scan",
     "Scanning library\u2026",
     "Bibliothek wird gescannt\u2026",
     "Analyse de la biblioth\u00e8que\u2026",
     "Scansione libreria\u2026"},
    {"sync.covers",
     "Downloading covers\u2026",
     "Cover werden geladen\u2026",
     "Chargement des couvertures\u2026",
     "Scaricamento copertine\u2026"},
    {"sync.cover_count", "%d / %d", "%d / %d", "%d / %d", "%d / %d"},
    {"sync.done", "Sync complete", "Sync abgeschlossen", "Sync termin\u00e9", "Sync completato"},
    {"sync.books", "%d books", "%d B\u00fccher", "%d livres", "%d libri"},
    {"status.fail", "Sync failed", "Sync fehlgeschlagen", "\u00c9chec du sync", "Sync fallito"},
    {"group.all", "All books", "Alle B\u00fccher", "Tous les livres", "Tutti i libri"},
    {"group.author", "By author", "Nach Autor", "Par auteur", "Per autore"},
    {"group.series", "By series", "Nach Reihe", "Par s\u00e9rie", "Per serie"},
    {"group.recent", "By recent", "Nach Neuheit", "Par date", "Per data"},
    {"group.year", "By year", "Nach Jahr", "Par ann\u00e9e", "Per anno"},
    {"group.genre", "By genre", "Nach Genre", "Par genre", "Per genere"},
    {"group.none.series", "No series", "Keine Reihe", "Sans s\u00e9rie", "Nessuna serie"},
    {"group.none.author", "Unknown author", "Unbekannter Autor", "Auteur inconnu", "Autore sconosciuto"},
    {"group.none.year", "Unknown year", "Unbekanntes Jahr", "Ann\u00e9e inconnue", "Anno sconosciuto"},
    {"group.none.genre", "Unknown genre", "Unbekanntes Genre", "Genre inconnu", "Genere sconosciuto"},
    {"action.group_by", "Group by", "Gruppieren nach", "Grouper par", "Raggruppa per"},
    {"group.none", "None", "Keine", "Aucune", "Nessuna"},
    {"group.author_series", "Author > Series", "Autor > Reihe", "Auteur > S\u00e9rie", "Autore > Serie"},
    {"action.sort_by", "Sort by", "Sortieren nach", "Trier par", "Ordina per"},
    {"sort.title_az", "Title A\u2013Z", "Titel A\u2013Z", "Titre A\u2013Z", "Titolo A\u2013Z"},
    {"sort.author", "By author", "Nach Autor", "Par auteur", "Per autore"},
    {"sort.series", "By series", "Nach Reihe", "Par s\u00e9rie", "Per serie"},
    {"sort.recent", "Recent", "Neuheiten", "R\u00e9cent", "Recenti"},
    {"view.grid", "Grid", "Raster", "Grille", "Griglia"},
    {"view.list", "List", "Liste", "Liste", "Elenco"},
    {"pager.info", "%d / %d", "%d / %d", "%d / %d", "%d / %d"},
    {"pager.prev", "<", "<", "<", "<"},
    {"pager.next", ">", ">", ">", ">"},
    {"pager.first", "<<", "<<", "<<", "<<"},
    {"pager.last", ">>", ">>", ">>", ">>"},
    {"action.settings", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.title", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.api_host", "API host", "API-Host", "H\u00f4te API", "Host API"},
    {"settings.api_key", "API key", "API-Schl\u00fcssel", "Cl\u00e9 API", "Chiave API"},
    {"settings.reader", "Reader app", "Lese-App", "Appli lecture", "App lettore"},
    {"settings.reader_auto", "Auto (server)", "Auto (Server)", "Auto (serveur)", "Auto (server)"},
    {"settings.dl_dir",
     "Download folder",
     "Download-Ordner",
     "Dossier de t\u00e9l\u00e9chargement",
     "Cartella download"},
    {"folder.select", "Select", "Ausw\u00e4hlen", "S\u00e9lectionner", "Seleziona"},
    {"folder.empty", "(empty)", "(leer)", "(vide)", "(vuota)"},
    {"source.title",
     "Library source",
     "Bibliotheksquelle",
     "Source de la biblioth\u00e8que",
     "Origine libreria"},
    {"source.kavita", "Kavita", "Kavita", "Kavita", "Kavita"},
    {"source.local", "Local", "Lokal", "Local", "Locale"},
    {"source.folder", "Folder", "Ordner", "Dossier", "Cartella"},
    {"settings.save", "Save & apply", "Speichern", "Enregistrer", "Salva e applica"},
    {"settings.logs", "Show logs", "Logs anzeigen", "Afficher les logs", "Mostra log"},
    {"settings.licenses", "Licenses", "Lizenzen", "Licences", "Licenze"},
    {"settings.system_app", "Install as system app", "Als System-App installieren", "Installer comme app système", "Installa come app di sistema"},
    {"settings.sysapp_on", "On (boots as home)", "An (als Startseite)", "Activé (s'ouvre en accueil)", "Sì (si avvia come home)"},
    {"settings.sysapp_off", "Off (app only)", "Aus (nur App)", "Désactivé (app uniquement)", "No (solo app)"},
    {"licenses.title", "Licenses", "Lizenzen", "Licences", "Licenze"},
    {"log.title", "Log", "Log", "Journal", "Log"},
    {"log.empty", "No log file yet", "Noch keine Logdatei", "Aucun journal", "Nessun log"},
    {"tab.search", "Search", "Suche", "Recherche", "Cerca"},
    {"settings.back", "Back", "Zur\u00fcck", "Retour", "Indietro"},
    {"settings.tap_edit", "tap to edit", "tippen", "toucher", "tocca"},
    {"settings.installed", "installed", "installiert", "install\u00e9e", "installata"},
    {"settings.not_installed", "not found", "nicht da", "absente", "assente"},
    {"tab.library", "Library", "Bibliothek", "Biblioth\u00e8que", "Libreria"},
    {"action.download_all",
     "Download all",
     "Alle herunterladen",
     "Tout t\u00e9l\u00e9charger",
     "Scarica tutto"},
    {"ctx.download", "Download", "Herunterladen", "T\u00e9l\u00e9charger", "Scarica"},
    {"ctx.open", "Open", "\u00d6ffnen", "Ouvrir", "Apri"},
    {"ctx.download_all",
     "Download all",
     "Alle herunterladen",
     "Tout t\u00e9l\u00e9charger",
     "Scarica tutto"},
    {"ctx.delete", "Delete", "L\u00f6schen", "Supprimer", "Elimina"},
    {"ctx.delete_series",
     "Delete series",
     "Reihe l\u00f6schen",
     "Supprimer la s\u00e9rie",
     "Elimina serie"},
    {"dl.failed", "Failed", "Fehlgeschlagen", "\u00c9chou\u00e9", "Fallito"},
    {"dl.in_progress",
     "Downloading\u2026",
     "L\u00e4dt\u2026",
     "T\u00e9l\u00e9chargement\u2026",
     "Download\u2026"},
    {"dl.progress",
     "Downloading %d / %d",
     "Lade %d / %d",
     "T\u00e9l\u00e9chargement %d / %d",
     "Download %d / %d"},
    {"dl.complete", "%d downloaded", "%d heruntergeladen", "%d t\u00e9l\u00e9charg\u00e9s", "%d scaricati"},
    {"dl.failed_count", "%d failed", "%d fehlgeschlagen", "%d \u00e9chec(s)", "%d falliti"},
    {"dl.title", "Download", "Download", "T\u00e9l\u00e9chargement", "Download"},
    {"dl.tap_close",
     "Tap to close",
     "Tippen zum Schlie\u00dfen",
     "Touchez pour fermer",
     "Tocca per chiudere"},
    {"action.apps", "Applications", "Anwendungen", "Applications", "Applicazioni"},
    {"launcher.title", "Applications", "Anwendungen", "Applications", "Applicazioni"},
    {"launcher.empty",
     "No applications",
     "Keine Anwendungen",
     "Aucune application",
     "Nessuna applicazione"},
    {NULL, NULL, NULL, NULL, NULL}};

const char *
eh_i18n(const char *key)
{
    /* Last-hit cache: the same keys are looked up on every frame
     * (labels/pager rows), so a content-compared memo of the previous
     * lookup avoids rescanning the ~80-entry table each call. */
    static char       g_i18n_cache_key[96];
    static char       g_i18n_cache_lang[8];
    static const char *g_i18n_cache_res;

    if (g_i18n_cache_res != NULL &&
        strcmp(g_i18n_cache_key, key) == 0 &&
        strcmp(g_i18n_cache_lang, eh_g_lang) == 0)
        return g_i18n_cache_res;

    const char *res = key;
    for (const BsI18n *e = eh_g_i18n; e->key != NULL; e++) {
        if (strcmp(e->key, key) == 0) {
            if (strcmp(eh_g_lang, "de") == 0 && e->de)
                res = e->de;
            else if (strcmp(eh_g_lang, "fr") == 0 && e->fr)
                res = e->fr;
            else if (strcmp(eh_g_lang, "it") == 0 && e->it)
                res = e->it;
            else
                res = e->en;
            break;
        }
    }
    snprintf(g_i18n_cache_key, sizeof g_i18n_cache_key, "%s", key);
    snprintf(g_i18n_cache_lang, sizeof g_i18n_cache_lang, "%s", eh_g_lang);
    g_i18n_cache_res = res;
    return res;
}
