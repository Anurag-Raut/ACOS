package dev.acos.fixtures.mediaprobe;

import android.app.Activity;
import android.media.MediaMetadata;
import android.media.session.MediaController;
import android.media.session.MediaSession;
import android.media.session.MediaSessionManager;
import android.media.session.PlaybackState;
import android.os.Bundle;
import android.os.Looper;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/**
 * The `media.session` proof fixture (MODFILL): a tiny media app.
 *
 * <p>It owns a real {@code MediaSession}: it publishes metadata and a playback
 * state and answers {@code MediaSession.Callback} like a music player (a
 * transport command changes its own state and republishes it). Every line is a
 * value the framework returned or a callback it ran:
 *
 * <ul>
 *   <li>{@code CMD <name> from=<package>} — the agent's play/pause/next/seek
 *       reached {@code MediaSession.Callback} (the package is what
 *       {@code getCurrentControllerInfo()} reports);</li>
 *   <li>{@code CTL ...} — a {@code MediaController} on the session's token
 *       read the state back through {@code ISessionController};</li>
 *   <li>{@code CTLCB ...} — a controller callback ran for a published change.</li>
 * </ul>
 */
public final class MainActivity extends Activity {
    private static final String TAG = "mediaprobe";
    private static final String[] TITLES = {"Blue in Green", "So What", "Freddie Freeloader"};

    private TextView status;
    private TextView title;
    private MediaSession session;
    private MediaController controller;
    private int track;
    private long position;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        title = new TextView(this);
        title.setId(R.id.title);
        title.setTextSize(20);
        title.setText("MEDIAPROBE");
        root.addView(title);

        root.addView(button("Create", view -> create()));
        root.addView(button("Publish", view -> publish(PlaybackState.STATE_PLAYING)));
        root.addView(button("Controller", view -> controllerRead()));
        root.addView(button("SelfPause", view -> {
            if (controller != null) {
                controller.getTransportControls().pause();
                log("SELF pause sent");
            }
        }));
        root.addView(button("Active", view -> active()));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);
        root.addView(status);
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);
        log("READY mediaprobe");
    }

    private Button button(String label, View.OnClickListener click) {
        Button button = new Button(this);
        button.setText(label);
        button.setGravity(Gravity.START);
        button.setOnClickListener(click);
        return button;
    }

    private void log(String line) {
        Log.i(TAG, line);
        Runnable append = () -> status.append(line + "\n");
        if (Looper.myLooper() == Looper.getMainLooper()) {
            append.run();
        } else {
            status.post(append);
        }
    }

    private String caller() {
        MediaSession.Callback unused = null;
        try {
            return session.getCurrentControllerInfo().getPackageName();
        } catch (Throwable failure) {
            return "unknown(" + failure.getClass().getSimpleName() + ")";
        }
    }

    private void create() {
        if (session != null) {
            log("CREATE already");
            return;
        }
        try {
            session = new MediaSession(this, "acos-music");
            session.setCallback(new MediaSession.Callback() {
                @Override
                public void onPlay() {
                    log("CMD play from=" + caller());
                    publish(PlaybackState.STATE_PLAYING);
                }

                @Override
                public void onPause() {
                    log("CMD pause from=" + caller());
                    publish(PlaybackState.STATE_PAUSED);
                }

                @Override
                public void onStop() {
                    log("CMD stop from=" + caller());
                    publish(PlaybackState.STATE_STOPPED);
                }

                @Override
                public void onSkipToNext() {
                    track = (track + 1) % TITLES.length;
                    position = 0;
                    log("CMD next from=" + caller() + " track=" + TITLES[track]);
                    publish(PlaybackState.STATE_PLAYING);
                }

                @Override
                public void onSkipToPrevious() {
                    track = (track + TITLES.length - 1) % TITLES.length;
                    position = 0;
                    log("CMD previous from=" + caller() + " track=" + TITLES[track]);
                    publish(PlaybackState.STATE_PLAYING);
                }

                @Override
                public void onSeekTo(long pos) {
                    position = pos;
                    log("CMD seek from=" + caller() + " pos=" + pos);
                    publish(PlaybackState.STATE_PLAYING);
                }

                @Override
                public void onCustomAction(String action, Bundle extras) {
                    log("CMD custom from=" + caller() + " action=" + action);
                }
            });
            session.setActive(true);
            controller = new MediaController(this, session.getSessionToken());
            controller.registerCallback(new MediaController.Callback() {
                @Override
                public void onPlaybackStateChanged(PlaybackState state) {
                    log("CTLCB state=" + (state == null ? "null" : name(state.getState()))
                            + " pos=" + (state == null ? -1 : state.getPosition()));
                }

                @Override
                public void onMetadataChanged(MediaMetadata metadata) {
                    log("CTLCB title=" + (metadata == null ? "null"
                            : metadata.getString(MediaMetadata.METADATA_KEY_TITLE)));
                }
            });
            log("CREATE ok tag=" + controller.getTag() + " pkg=" + controller.getPackageName());
        } catch (Throwable failure) {
            log("CREATE threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void publish(int state) {
        if (session == null) {
            log("PUBLISH no-session");
            return;
        }
        try {
            session.setMetadata(new MediaMetadata.Builder()
                    .putString(MediaMetadata.METADATA_KEY_TITLE, TITLES[track])
                    .putString(MediaMetadata.METADATA_KEY_ARTIST, "Miles Davis")
                    .putLong(MediaMetadata.METADATA_KEY_DURATION, 337000L)
                    .build());
            session.setPlaybackState(new PlaybackState.Builder()
                    .setState(state, position, 1.0f)
                    .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
                            | PlaybackState.ACTION_SKIP_TO_NEXT
                            | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SEEK_TO)
                    .build());
            log("PUBLISH state=" + name(state) + " track=" + TITLES[track]
                    + " pos=" + position);
        } catch (Throwable failure) {
            log("PUBLISH threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void controllerRead() {
        if (controller == null) {
            log("CTL no-controller");
            return;
        }
        try {
            PlaybackState state = controller.getPlaybackState();
            MediaMetadata metadata = controller.getMetadata();
            log("CTL state=" + (state == null ? "null" : name(state.getState()))
                    + " pos=" + (state == null ? -1 : state.getPosition())
                    + " actions=" + (state == null ? 0 : state.getActions())
                    + " title=" + (metadata == null ? "null"
                            : metadata.getString(MediaMetadata.METADATA_KEY_TITLE))
                    + " artist=" + (metadata == null ? "null"
                            : metadata.getString(MediaMetadata.METADATA_KEY_ARTIST))
                    + " duration=" + (metadata == null ? -1
                            : metadata.getLong(MediaMetadata.METADATA_KEY_DURATION)));
        } catch (Throwable failure) {
            log("CTL threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    /** `getActiveSessions` needs a notification-listener grant on a phone and the
     *  service answers a structured gap here: the line records which. */
    private void active() {
        try {
            MediaSessionManager manager = getSystemService(MediaSessionManager.class);
            log("ACTIVE n=" + manager.getActiveSessions(null).size());
        } catch (Throwable failure) {
            log("ACTIVE threw=" + failure.getClass().getSimpleName());
        }
    }

    private static String name(int state) {
        switch (state) {
            case PlaybackState.STATE_PLAYING: return "PLAYING";
            case PlaybackState.STATE_PAUSED: return "PAUSED";
            case PlaybackState.STATE_STOPPED: return "STOPPED";
            case PlaybackState.STATE_NONE: return "NONE";
            default: return "OTHER" + state;
        }
    }
}
