1. Ada management ai = menggunakan 9router (https://9router.com/  perintah install npm install -g 9router  , perintah start 9router)
2. AI di Openclaw pakai dari 9router, via konfigurasi 9router
3. 
4. Ada list command pada aplikasi untuk terhubung ke agent ini(start, pilih proyek, preview, test, change model ai, push github)
5. 1 User di kelola dalam 1 container docker, supaya enak mecahnya
6. Login ke agent ini, menggunakan apikey
8. Ada akses khusus admin ke agent ini
9. Admin agent, bisa membuat kontainer docker dengan tujuan membuatkan user yang akan pakai agent ini (atau user saat akses petama agent ini, auto create kontainer)
user ini pakai apikey, jadi saat buat apikey maka buat kontainer, jika delete berarti delete kontainer
10. Ada tracking penggunaan token dan cost sesuai model yang dipakai (per apikey)
11. Ada konfigurasi interaktif di saat setup dan ada command untuk akses menu ( list menu = start 9router, stop 9router, open 9router, setup openclaw)




Flow App ---->> Dealtech Code

Aplikasi -> Login -> Open menu AI Coding -> Input API Key -> Connect Github -> Bisa clone / create repo -> start coding 