/* bs_i18n.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── i18n ────────────────────────────────────────────────────────────── */

char g_lang[8] = "en";

/* Trivial i18n table.  Key = English string; value = translation.
 * Add rows here for new languages.  Falls back to English on miss.
 */

const I18n g_i18n[] = {
    {"app.title", "Bookshelf", "B\u00fccherregal", "\u00c9tag\u00e8re", "pbemu libreria"},
    {"action.sync", "Sync", "Sync", "Sync", "Sync"},
    {"action.more", "More", "Mehr", "Plus", "Altro"},
    {"action.menu", "Menu", "Men\u00fc", "Menu", "Menu"},
    {"search.ph", "search\u2026", "suchen\u2026", "rechercher\u2026", "cerca\u2026"},
    {"search.empty",
     "No recent searches",
     "Keine letzten Suchen",
     "Aucune recherche r\u00e9cente",
     "Nessuna ricerca recente"},
    {"status.idle",
     "Tap \u21bb to sync",
     "Tippe \u21bb zum Sync",
     "Touchez \u21bb",
     "Tocca \u21bb"},
    {"status.syncing",
     "Syncing\u2026",
     "Sync l\u00e4uft\u2026",
     "Sync\u2026",
     "Sincronizzando\u2026"},
    {"status.done", "%d book(s)", "%d Buch/B\u00fccher", "%d livre(s)", "%d libro/i"},
    {"status.fail", "Sync failed", "Sync fehlgeschlagen", "\u00c9chec du sync", "Sync fallito"},
    {"status.no_books", "No books yet", "Noch keine B\u00fccher", "Pas de livres", "Nessun libro"},
    {"status.search_no", "No matches", "Keine Treffer", "Aucun r\u00e9sultat", "Nessun risultato"},
    {"group.all", "All books", "Alle B\u00fccher", "Tous les livres", "Tutti i libri"},
    {"group.author", "By author", "Nach Autor", "Par auteur", "Per autore"},
    {"group.series", "By series", "Nach Reihe", "Par s\u00e9rie", "Per serie"},
    {"group.recent", "By recent", "Nach Neuheit", "Par date", "Per data"},
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
    {"filter.all", "All", "Alle", "Tous", "Tutti"},
    {"filter.dl", "Downloaded", "Heruntergeladen", "T\u00e9l\u00e9charg\u00e9s", "Scaricati"},
    {"filter.rd", "Remote only", "Nur Remote", "Distant seulement", "Solo remoti"},
    {"action.settings", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.title", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.api_host", "API host", "API-Host", "H\u00f4te API", "Host API"},
    {"settings.api_key", "API key", "API-Schl\u00fcssel", "Cl\u00e9 API", "Chiave API"},
    {"settings.reader", "Reader app", "Lese-App", "Appli lecture", "App lettore"},
    {"settings.reader_auto", "Auto (server)", "Auto (Server)", "Auto (serveur)", "Auto (server)"},
    {"settings.save", "Save & apply", "Speichern", "Enregistrer", "Salva e applica"},
    {"tab.search", "Search", "Suche", "Recherche", "Cerca"},
    {"settings.back", "Back", "Zur\u00fcck", "Retour", "Indietro"},
    {"settings.tap_edit", "tap to edit", "tippen", "toucher", "tocca"},
    {"settings.installed", "installed", "installiert", "install\u00e9e", "installata"},
    {"settings.not_installed", "not found", "nicht da", "absente", "assente"},
    {"tab.library", "Library", "Bibliothek", "Biblioth\u00e8que", "Libreria"},
    {"action.download_all",
     "Download all",
     "Alle laden",
     "Tout t\u00e9l\u00e9charger",
     "Scarica tutto"},
    {"ctx.download", "Download", "Laden", "T\u00e9l\u00e9charger", "Scarica"},
    {"ctx.open", "Open", "\u00d6ffnen", "Ouvrir", "Apri"},
    {"ctx.download_all",
     "Download all",
     "Alle laden",
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
    {"dl.complete", "%d downloaded", "%d geladen", "%d t\u00e9l\u00e9charg\u00e9s", "%d scaricati"},
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
    {"launcher.back", "Back", "Zurück", "Retour", "Indietro"},
    {NULL, NULL, NULL, NULL, NULL}};

const char *
i18n(const char *key)
{
    for (const I18n *e = g_i18n; e->key != NULL; e++) {
        if (strcmp(e->key, key) == 0) {
            if (strcmp(g_lang, "de") == 0 && e->de)
                return e->de;
            if (strcmp(g_lang, "fr") == 0 && e->fr)
                return e->fr;
            if (strcmp(g_lang, "it") == 0 && e->it)
                return e->it;
            return e->en;
        }
    }
    return key;
}
