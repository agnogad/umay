/*
 * Minimal GTK3 + WebKit2GTK 4.1 surface for the visible UMAY GUI demo.
 * All handles are void*; every argument is scalar or pointer so `umay bindgen`
 * can emit it. `usleep` lets the event pump yield without SDL.
 */
void gtk_init(int *argc, char ***argv);
void *gtk_window_new(int window_type);
void *webkit_web_view_new(void);
void gtk_container_add(void *container, void *widget);
void webkit_web_view_load_uri(void *webview, const char *uri);
void gtk_window_set_title(void *window, const char *title);
void gtk_window_set_default_size(void *window, int width, int height);
void gtk_widget_show_all(void *widget);
int gtk_events_pending(void);
int gtk_main_iteration(void);
void gtk_widget_destroy(void *widget);
void usleep(unsigned int usec);
