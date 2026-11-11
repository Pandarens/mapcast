package app.mapcast;

import android.app.Activity;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.EditorInfo;
import android.webkit.CookieManager;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;

/**
 * MapCast for Android: the companion's map page full screen, with the screen kept on, and the
 * PC found on the Wi-Fi by itself (the companion answers a UDP broadcast with its address).
 * The page asks for the token once; the WebView keeps it in a cookie.
 */
public class MainActivity extends Activity {
    private static final int PORT = 8765;
    private static final byte[] QUESTION = "MAPCAST?".getBytes(StandardCharsets.US_ASCII);
    private static final String ANSWER = "MAPCAST ";
    private static final int BACKGROUND = 0xFF16130F;
    private static final int ACCENT = 0xFFE0B04A;

    private final Handler ui = new Handler(Looper.getMainLooper());
    private SharedPreferences prefs;
    private WebView web;
    private LinearLayout setup;
    private TextView status;
    private EditText address;
    private String opened;
    private volatile int searchId;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Window window = getWindow();
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams lp = window.getAttributes();
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(lp);
        }
        prefs = getSharedPreferences("mapcast", MODE_PRIVATE);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(BACKGROUND);
        web = makeWebView();
        root.addView(web, new FrameLayout.LayoutParams(-1, -1));
        setup = makeSetup();
        root.addView(setup, new FrameLayout.LayoutParams(-1, -1));
        setContentView(root);
        hideSystemBars();

        String last = prefs.getString("url", null);
        if (last != null) {
            open(last);
        } else {
            showSetup(null);
            search();
        }
    }

    private WebView makeWebView() {
        WebView view = new WebView(this);
        view.setBackgroundColor(BACKGROUND);
        WebSettings s = view.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setCacheMode(WebSettings.LOAD_DEFAULT);
        s.setMediaPlaybackRequiresUserGesture(true);
        s.setSupportZoom(false);
        s.setBuiltInZoomControls(false);
        CookieManager.getInstance().setAcceptCookie(true);
        view.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest request) {
                return false; // the page and its login stay inside the app
            }

            @Override
            public void onReceivedError(WebView v, WebResourceRequest request, WebResourceError error) {
                if (request.isForMainFrame()) {
                    showSetup("Не открылся " + opened + ". Компаньон на ПК запущен?");
                    search();
                }
            }

            @Override
            public void onPageFinished(WebView v, String url) {
                CookieManager.getInstance().flush();
            }
        });
        return view;
    }

    private LinearLayout makeSetup() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setGravity(Gravity.CENTER);
        box.setBackgroundColor(BACKGROUND);
        int pad = dp(28);
        box.setPadding(pad, pad, pad, pad);

        TextView title = new TextView(this);
        title.setText("MapCast");
        title.setTextColor(ACCENT);
        title.setTextSize(28);
        title.setGravity(Gravity.CENTER);
        box.addView(title, wrap());

        status = new TextView(this);
        status.setTextColor(0xFFF1E9D8);
        status.setTextSize(16);
        status.setGravity(Gravity.CENTER);
        status.setPadding(0, dp(16), 0, dp(20));
        box.addView(status, wrap());

        address = new EditText(this);
        address.setHint("адрес из окна компаньона, например 192.168.1.10:8765");
        address.setHintTextColor(0xFF8D8579);
        address.setTextColor(Color.WHITE);
        address.setSingleLine(true);
        address.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        address.setImeOptions(EditorInfo.IME_ACTION_GO);
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int actionId, KeyEvent event) {
                openTyped();
                return true;
            }
        });
        box.addView(address, wrap());

        Button go = button("Открыть");
        go.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                openTyped();
            }
        });
        box.addView(go, wrap());

        Button again = button("Искать ПК снова");
        again.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                search();
            }
        });
        box.addView(again, wrap());
        return box;
    }

    private Button button(String text) {
        Button b = new Button(this);
        b.setText(text);
        b.setAllCaps(false);
        b.setTextSize(16);
        return b;
    }

    private LinearLayout.LayoutParams wrap() {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(-1, -2);
        lp.topMargin = dp(6);
        return lp;
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    private void openTyped() {
        String text = address.getText().toString().trim();
        if (text.isEmpty()) return;
        if (!text.startsWith("http://") && !text.startsWith("https://")) text = "http://" + text;
        String rest = text.substring(text.indexOf("://") + 3);
        String host = rest.split("[/?]", 2)[0];
        if (!host.contains(":")) text = text.replaceFirst(java.util.regex.Pattern.quote(host), host + ":" + PORT);
        open(text);
    }

    private void open(String url) {
        searchId++; // a search still running is no longer wanted
        opened = url;
        prefs.edit().putString("url", url).apply();
        setup.setVisibility(View.GONE);
        web.setVisibility(View.VISIBLE);
        web.loadUrl(url);
        hideSystemBars();
    }

    private void showSetup(String message) {
        web.setVisibility(View.INVISIBLE);
        setup.setVisibility(View.VISIBLE);
        if (message != null) status.setText(message);
        String last = prefs.getString("url", null);
        if (last != null && address.getText().length() == 0) {
            address.setText(last.replaceFirst("^https?://", "").replaceFirst("/.*$", ""));
        }
    }

    /** Ask the network for the companion: a broadcast it answers with its page's address. */
    private void search() {
        final int id = ++searchId;
        status.setText("Ищу компаньон на ПК в этой Wi-Fi сети…");
        new Thread(new Runnable() {
            @Override
            public void run() {
                lookFor(id);
            }
        }, "search").start();
    }

    private void lookFor(final int id) {
        {
            String found = null;
            try (DatagramSocket socket = new DatagramSocket()) {
                socket.setBroadcast(true);
                socket.setSoTimeout(700);
                byte[] buf = new byte[256];
                for (int attempt = 0; attempt < 5 && found == null && id == searchId; attempt++) {
                    for (InetAddress target : targets()) {
                        try {
                            socket.send(new DatagramPacket(QUESTION, QUESTION.length, target, PORT));
                        } catch (Exception ignored) {
                            // one address may not take a broadcast; the others might
                        }
                    }
                    long until = System.currentTimeMillis() + 1400;
                    while (found == null && System.currentTimeMillis() < until) {
                        try {
                            DatagramPacket reply = new DatagramPacket(buf, buf.length);
                            socket.receive(reply);
                            String text = new String(reply.getData(), 0, reply.getLength(), StandardCharsets.UTF_8);
                            if (text.startsWith(ANSWER)) found = text.substring(ANSWER.length()).trim();
                        } catch (java.net.SocketTimeoutException timeout) {
                            break;
                        }
                    }
                }
            } catch (Exception e) {
                found = null;
            }
            final String url = found;
            ui.post(new Runnable() {
                @Override
                public void run() {
                    if (id != searchId) return;
                    if (url != null) {
                        open(url);
                    } else {
                        status.setText("Не нашёл компаньон. Он запущен на ПК, телефон в той же Wi-Fi сети? "
                                + "Можно ввести адрес из окна компаньона.");
                    }
                }
            });
        }
    }

    private java.util.List<InetAddress> targets() {
        java.util.List<InetAddress> out = new java.util.ArrayList<>();
        try {
            WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(WIFI_SERVICE);
            if (wifi != null && wifi.getDhcpInfo() != null) {
                int ip = wifi.getDhcpInfo().ipAddress;
                int mask = wifi.getDhcpInfo().netmask;
                if (ip != 0 && mask != 0) {
                    int b = (ip & mask) | ~mask; // the Wi-Fi subnet's broadcast address
                    out.add(InetAddress.getByAddress(new byte[] {
                            (byte) b, (byte) (b >> 8), (byte) (b >> 16), (byte) (b >> 24) }));
                }
            }
        } catch (Exception ignored) {
            // no Wi-Fi details: the general broadcast below still goes out
        }
        try {
            out.add(InetAddress.getByName("255.255.255.255"));
        } catch (Exception ignored) {
        }
        return out;
    }

    private void hideSystemBars() {
        Window window = getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
            WindowInsetsController c = window.getInsetsController();
            if (c != null) {
                c.hide(WindowInsets.Type.systemBars());
                c.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        } else {
            window.getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                    | View.SYSTEM_UI_FLAG_FULLSCREEN | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_LAYOUT_STABLE | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) hideSystemBars();
    }

    @Override
    protected void onResume() {
        super.onResume();
        web.onResume();
        web.resumeTimers();
        hideSystemBars();
    }

    @Override
    protected void onPause() {
        web.onPause();
        web.pauseTimers();
        CookieManager.getInstance().flush();
        super.onPause();
    }

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (keyCode == KeyEvent.KEYCODE_BACK && web.getVisibility() == View.VISIBLE) {
            // back from the map: the screen to change the PC (back again leaves the app)
            showSetup("Подключено к " + opened);
            return true;
        }
        return super.onKeyDown(keyCode, event);
    }

    @Override
    protected void onDestroy() {
        searchId++;
        web.destroy();
        super.onDestroy();
    }
}
