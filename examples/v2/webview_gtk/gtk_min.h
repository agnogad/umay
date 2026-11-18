/*
 * Minimal GTK3 / WebKit2GTK 4.1 surface for the Phase 5/6 webview shell spike.
 *
 * Deliberately a SUBSET of <gtk/gtk.h> and <webkit/webkitwebview.h>: only the
 * declarations the app calls, and only types `umay bindgen` accepts. Every
 * GObject handle (GtkWidget, GtkWindow, WebKitWebView) is reduced to `void*`,
 * and every C enum/gboolean to `int`. That is ABI-safe on every supported
 * target: an object pointer and `void*` have the same width, `gboolean` is
 * `gint` (a plain C int), and GTK3's GtkWindowType is a C enum passed as int.
 * Using `void*` also removes the need for the GObject type-cast macros
 * (GTK_WINDOW(), WEBKIT_WEB_VIEW()), which a macro-less header reader cannot
 * express.
 *
 * Real signatures verified in /usr/include (GTK 3.24 / WebKit2GTK 4.1):
 *   void     gtk_init(int *argc, char ***argv)                 -> as written
 *   GtkWidget *gtk_window_new(GtkWindowType type)              -> void*, int
 *   GtkWidget *webkit_web_view_new(void)                       -> void*
 *   void     gtk_container_add(GtkContainer *c, GtkWidget *w)  -> void*, void*
 *   void     webkit_web_view_load_uri(WebKitWebView *v,
 *                                     const gchar *uri)        -> void*, const char*
 *   void     gtk_widget_show_all(GtkWidget *w)                 -> void*
 *   gboolean gtk_events_pending(void)                          -> int
 *   gboolean gtk_main_iteration(void)                          -> int
 *   void     gtk_widget_destroy(GtkWidget *w)                  -> void*
 *
 * `gtk_init(NULL, NULL)` is supported by GTK3: gtk_parse_args accepts a null
 * argc/argv pair, so no argv plumbing is needed. The shell uses a BOUNDED
 * gtk_events_pending/gtk_main_iteration pump instead of gtk_main() (which never
 * returns) and needs no signal callbacks for this first slice. The JS bridge
 * (g_signal_connect / script-message-received) is deferred.
 *
 * No hand-written UMAY FFI: the .um bindings come from
 * `umay bindgen --header gtk_min.h --out gtk_gen.um`.
 */
void gtk_init(int *argc, char ***argv);
void *gtk_window_new(int window_type);
void *webkit_web_view_new(void);
void gtk_container_add(void *container, void *widget);
void webkit_web_view_load_uri(void *webview, const char *uri);
void gtk_widget_show_all(void *widget);
int gtk_events_pending(void);
int gtk_main_iteration(void);
void gtk_widget_destroy(void *widget);
