import android.os.Bundle;
import android.os.Handler;
import android.view.KeyEvent;
import android.view.inputmethod.CompletionInfo;
import android.view.inputmethod.CorrectionInfo;
import android.view.inputmethod.ExtractedText;
import android.view.inputmethod.ExtractedTextRequest;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputContentInfo;

/**
 * Runtime-level harness for {@code acos_runtime::text_java}: a real Java object
 * implementing the real framework {@link InputConnection} interface, so the
 * Rust bridge's {@code commitText(CharSequence, int)} call is exercised through
 * the actual Java method dispatch and argument marshalling.
 *
 * <p>The text-input fixture's {@code Activity} cannot launch on this HEAD (the
 * `VectorDrawable` blocker, W68-A), so this harness supplies the connection the
 * framework's own {@code EditableInputConnection} would otherwise provide. It
 * records the committed text and cursor, which the Rust test reads back.
 */
public final class TextInputMain {
    /** The connection the Rust test drives. */
    public static InputConnection connection;

    /** Last {@code commitText} payload. */
    public static String committed;

    /** Last {@code commitText} newCursorPosition. */
    public static int cursor = -999;

    /** Number of {@code commitText} calls. */
    public static int commits;

    /** Number of {@code setComposingText} calls (must stay 0 for an IME commit). */
    public static int composing;

    /** An empty field; the harness is about the commit path, not extraction. */
    static final class Field implements InputConnection {
        @Override public CharSequence getTextBeforeCursor(int n, int flags) { return ""; }
        @Override public CharSequence getTextAfterCursor(int n, int flags) { return ""; }
        @Override public CharSequence getSelectedText(int flags) { return ""; }
        @Override public int getCursorCapsMode(int reqModes) { return 0; }
        @Override public ExtractedText getExtractedText(ExtractedTextRequest r, int flags) { return null; }
        @Override public boolean deleteSurroundingText(int before, int after) { return true; }
        @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) { return true; }
        @Override public boolean setComposingText(CharSequence text, int newCursor) {
            composing++;
            return true;
        }
        @Override public boolean setComposingRegion(int start, int end) { return true; }
        @Override public boolean finishComposingText() { return true; }
        @Override public boolean commitText(CharSequence text, int newCursor) {
            committed = text == null ? null : text.toString();
            cursor = newCursor;
            commits++;
            return true;
        }
        @Override public boolean commitCompletion(CompletionInfo text) { return true; }
        @Override public boolean commitCorrection(CorrectionInfo info) { return true; }
        @Override public boolean setSelection(int start, int end) { return true; }
        @Override public boolean performEditorAction(int actionCode) { return true; }
        @Override public boolean performContextMenuAction(int id) { return true; }
        @Override public boolean beginBatchEdit() { return true; }
        @Override public boolean endBatchEdit() { return true; }
        @Override public boolean sendKeyEvent(KeyEvent event) { return true; }
        @Override public boolean clearMetaKeyStates(int states) { return true; }
        @Override public boolean reportFullscreenMode(boolean enabled) { return true; }
        @Override public boolean performPrivateCommand(String action, Bundle data) { return true; }
        @Override public boolean requestCursorUpdates(int cursorUpdateMode) { return true; }
        @Override public Handler getHandler() { return null; }
        @Override public void closeConnection() {}
        @Override public boolean commitContent(InputContentInfo info, int flags, Bundle opts) {
            return true;
        }
    }

    public static void main(String[] args) {
        connection = new Field();
        System.out.println("textinput-ready");
    }
}
