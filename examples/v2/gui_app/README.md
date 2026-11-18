# UMAY Notlar — gerçek WebKitGTK GUI demosu

Gerçek bir GTK 3 penceresi içinde WebKitGTK 4.1 webview ile çalışan HTML tabanlı
not uygulaması. FFI bildirimlerinin tamamı `umay bindgen` çıktısıdır; elle extern
yazılmaz.

## Dosyalar

- `gtk_min.h` — GTK3 + WebKit2GTK + `usleep` için dar C başlığı (yalnızca
  skaler/işaretçi argümanlar).
- `gtk_gen.um` — `umay bindgen` çıktısı (elle düzenlemeyin).
- `app.html` — HTML/CSS/JS not arayüzü (ekleme, işaretleme, silme).
- `app.um` — uygulama gövdesi; `gtk_gen.um` + `stdlib/ipc.um` + `app.um`
  sırayla tek bir `main.um` içine eklenir (modül içi extern'ler özel ve sembol
  adları `mod::fn` olduğu için splice gerekir).

## Çalıştırma

```sh
cd /tmp && mkdir -p umay-gui && cd umay-gui
cat $REPO/examples/v2/gui_app/gtk_gen.um \
    $REPO/stdlib/ipc.um \
    $REPO/examples/v2/gui_app/app.um > main.um
$REPO/target/debug/umay build main.um
GDK_BACKEND=x11 DISPLAY=:0 ./dist/main
```

`umay build`, assembly'de `gtk_`/`webkit_` çağrılarını görünce gerekli 18 link
bayrağını otomatik ekler. Uygulama 6 saniye açık kalır, ardından tek satır
`{"id":7,"ok":true,"result":4}` yazıp kapanır.

Ekran görüntüsü almak için (ör. Xvfb):

```sh
Xvfb :99 -screen 0 1280x800x24 &
DISPLAY=:99 GDK_BACKEND=x11 ./dist/main &
sleep 3
ffmpeg -f x11grab -video_size 1280x800 -i :99.0 -frames:v 1 gui.png
```

> Not: Bu ortamda GTK varsayılan olarak Wayland backend'ini seçiyor ve pencere
> X11 ekran görüntüsünde görünmüyor. `GDK_BACKEND=x11` zorunludur.

## Sınırlar

JS → UMAY köprüsü henüz callback gerektirdiği için yok; HTML kendi içinde
etkileşimlidir, UMAY tarafı yalnızca pencereyi/IPC yanıtını yönetir. `file://`
varlık yolu demo amaçlıdır; paketlenmiş uygulamada `umay://app/` şeması
kullanılmalıdır.
