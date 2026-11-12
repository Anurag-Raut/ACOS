package dev.acos.fixtures.settletab;

import android.animation.Animator;
import android.animation.AnimatorListenerAdapter;
import android.animation.LayoutTransition;
import android.animation.ValueAnimator;
import android.app.Activity;
import android.app.Dialog;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.ProgressBar;
import android.widget.TextView;

import java.util.ArrayList;

/**
 * settle-lab: one screen per kind of work the strict settle rule must wait for.
 *
 * <p>Every case has a deterministic final text/state, so a reply that comes
 * back earlier than that state is an observable false pass:
 * <ul>
 *   <li>300 ms ValueAnimator: anim_text -> "anim-done" at the end;</li>
 *   <li>postDelayed 40/200/1000 ms: delayN_text -> "delayN-done";</li>
 *   <li>background thread 150 ms then runOnUiThread: bg_text -> "bg-done";</li>
 *   <li>three chained View.post hops: chain_text -> "chain-done";</li>
 *   <li>fade Dialog: dialog_text -> "dialog-open";</li>
 *   <li>Activity transition: second_text -> "second-open";</li>
 *   <li>ListView smooth scroll: list_status -> "list-idle" with row-20 visible;</li>
 *   <li>indeterminate ProgressBar: never settles (the honest result is timeout);</li>
 *   <li>LayoutTransition add: lt_text -> "lt-added" after the transition;</li>
 *   <li>rapid counter: rapid_count -> "10" after ten taps.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private final Handler handler = new Handler(Looper.getMainLooper());
    private int rapid = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);

        final TextView animText = (TextView) findViewById(R.id.anim_text);
        final TextView delay40Text = (TextView) findViewById(R.id.delay40_text);
        final TextView delay200Text = (TextView) findViewById(R.id.delay200_text);
        final TextView delay1000Text = (TextView) findViewById(R.id.delay1000_text);
        final TextView delay1000Done = (TextView) findViewById(R.id.delay1000_done);
        final TextView bgText = (TextView) findViewById(R.id.bg_text);
        final TextView chainText = (TextView) findViewById(R.id.chain_text);
        final TextView windowText = (TextView) findViewById(R.id.window_text);
        final TextView listStatus = (TextView) findViewById(R.id.list_status);
        final TextView rapidCount = (TextView) findViewById(R.id.rapid_count);
        final ListView list = (ListView) findViewById(R.id.fling_list);
        final ProgressBar progress = (ProgressBar) findViewById(R.id.progress_indeterminate);
        final LinearLayout ltContainer = (LinearLayout) findViewById(R.id.lt_container);

        ArrayList<String> rows = new ArrayList<String>();
        for (int i = 0; i < 100; i++) rows.add("row-" + i);
        list.setAdapter(new ArrayAdapter<String>(
                this, android.R.layout.simple_list_item_1, rows));

        final LayoutTransition transition = new LayoutTransition();
        transition.enableTransitionType(LayoutTransition.APPEARING);
        transition.enableTransitionType(LayoutTransition.CHANGING);
        transition.enableTransitionType(LayoutTransition.DISAPPEARING);
        ltContainer.setLayoutTransition(transition);

        findViewById(R.id.anim_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                animText.setText("anim-run");
                animText.setAlpha(1f);
                ValueAnimator a = ValueAnimator.ofFloat(1f, 0.2f);
                a.setDuration(300);
                a.addUpdateListener(new ValueAnimator.AnimatorUpdateListener() {
                    @Override public void onAnimationUpdate(ValueAnimator animation) {
                        animText.setAlpha((Float) animation.getAnimatedValue());
                    }
                });
                a.addListener(new AnimatorListenerAdapter() {
                    @Override public void onAnimationEnd(Animator animation) {
                        animText.setAlpha(1f);
                        animText.setText("anim-done");
                    }
                });
                a.start();
            }
        });

        findViewById(R.id.delay40).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                delay40Text.setText("delay40-wait");
                handler.postDelayed(new Runnable() {
                    @Override public void run() { delay40Text.setText("delay40-done"); }
                }, 40);
            }
        });
        findViewById(R.id.delay200).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                delay200Text.setText("delay200-wait");
                handler.postDelayed(new Runnable() {
                    @Override public void run() { delay200Text.setText("delay200-done"); }
                }, 200);
            }
        });
        findViewById(R.id.delay1000).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                delay1000Text.setText("delay1000-wait");
                delay1000Done.setVisibility(View.GONE);
                handler.postDelayed(new Runnable() {
                    @Override public void run() {
                        delay1000Text.setText("delay1000-done");
                        delay1000Done.setVisibility(View.VISIBLE);
                    }
                }, 1000);
            }
        });

        findViewById(R.id.bg_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                bgText.setText("bg-wait");
                new Thread(new Runnable() {
                    @Override public void run() {
                        try { Thread.sleep(150); } catch (InterruptedException ignored) { }
                        runOnUiThread(new Runnable() {
                            @Override public void run() { bgText.setText("bg-done"); }
                        });
                    }
                }, "settle-lab-bg").start();
            }
        });

        findViewById(R.id.chain_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                chainText.setText("chain-1");
                chainText.post(new Runnable() {
                    @Override public void run() {
                        chainText.setText("chain-2");
                        chainText.post(new Runnable() {
                            @Override public void run() { chainText.setText("chain-done"); }
                        });
                    }
                });
            }
        });

        findViewById(R.id.dialog_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                final Dialog dialog = new Dialog(MainActivity.this);
                dialog.setTitle("Settle dialog");
                dialog.setContentView(R.layout.dialog);
                dialog.findViewById(R.id.dialog_close).setOnClickListener(
                        new View.OnClickListener() {
                            @Override public void onClick(View view) { dialog.dismiss(); }
                        });
                dialog.show();
                windowText.setText("dialog-open");
            }
        });

        findViewById(R.id.activity_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                startActivity(new Intent(MainActivity.this, SecondActivity.class));
            }
        });

        findViewById(R.id.list_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                list.setVisibility(View.VISIBLE);
                listStatus.setText("list-scrolling");
                list.setSelection(0);
                list.post(new Runnable() {
                    @Override public void run() {
                        list.fling(3000);
                        // Frame-driven: after three frames with the same first
                        // visible position, the fling is over. The marker is app
                        // state, and every postOnAnimation callback is a real
                        // Choreographer frame the settle rule must wait for.
                        final Runnable settlePoll = new Runnable() {
                            int lastPos = -1;
                            int lastTop = 0;
                            int stable = 0;
                            @Override public void run() {
                                int pos = list.getFirstVisiblePosition();
                                View child = list.getChildAt(0);
                                int top = child == null ? 0 : child.getTop();
                                if (pos == lastPos && top == lastTop) {
                                    stable++;
                                } else {
                                    stable = 0;
                                    lastPos = pos;
                                    lastTop = top;
                                }
                                if (stable >= 3) {
                                    listStatus.setText("list-idle");
                                } else {
                                    list.postOnAnimation(this);
                                }
                            }
                        };
                        list.post(settlePoll);
                    }
                });
            }
        });

        findViewById(R.id.progress_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                progress.setIndeterminate(true);
                progress.setVisibility(View.VISIBLE);
            }
        });
        findViewById(R.id.progress_stop).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                progress.setVisibility(View.GONE);
            }
        });

        findViewById(R.id.lt_start).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                ltContainer.removeAllViews();
                TextView added = new TextView(MainActivity.this);
                added.setId(R.id.lt_text);
                added.setText("lt-added");
                added.setTextSize(12f);
                ltContainer.addView(added);
            }
        });

        findViewById(R.id.rapid_inc).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                rapid++;
                rapidCount.setText(String.valueOf(rapid));
            }
        });
        findViewById(R.id.rapid_reset).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                rapid = 0;
                rapidCount.setText("0");
            }
        });
    }
}
