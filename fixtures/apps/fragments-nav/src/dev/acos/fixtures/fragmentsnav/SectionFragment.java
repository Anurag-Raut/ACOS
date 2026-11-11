package dev.acos.fixtures.fragmentsnav;

import android.app.Activity;
import android.app.Fragment;
import android.os.Bundle;
import android.util.Log;
import android.view.LayoutInflater;
import android.view.View;
import android.view.ViewGroup;
import android.widget.TextView;

/**
 * A plain framework {@link android.app.Fragment} (not the AndroidX one) used by
 * {@link MainActivity}. Shows its title plus a tap counter kept in instance
 * state.
 */
public class SectionFragment extends Fragment {
    public static final String TAG = "AcosFragments";
    public static final String ARG_TITLE = "title";
    private static final String KEY_TAPS = "taps";

    private int taps;
    private TextView text;

    public static SectionFragment newInstance(String title) {
        SectionFragment fragment = new SectionFragment();
        Bundle args = new Bundle();
        args.putString(ARG_TITLE, title);
        fragment.setArguments(args);
        return fragment;
    }

    private String title() {
        Bundle args = getArguments();
        return args == null ? "?" : args.getString(ARG_TITLE, "?");
    }

    @Override
    public View onCreateView(LayoutInflater inflater, ViewGroup container,
            Bundle savedInstanceState) {
        if (savedInstanceState != null) {
            taps = savedInstanceState.getInt(KEY_TAPS, 0);
        }
        View view = inflater.inflate(R.layout.fragment, container, false);
        text = (TextView) view.findViewById(R.id.frag_text);
        render();
        view.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                taps++;
                render();
                Log.i(TAG, "fragment_tap title=" + title() + " taps=" + taps);
            }
        });
        Log.i(TAG, "fragment_view title=" + title() + " taps=" + taps);
        return view;
    }

    private void render() {
        text.setText(title() + " taps=" + taps);
    }

    @Override
    public void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        outState.putInt(KEY_TAPS, taps);
        Log.i(TAG, "fragment_save title=" + title() + " taps=" + taps);
    }
}
