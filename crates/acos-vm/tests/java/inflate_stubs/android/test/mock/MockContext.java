package android.test.mock;

/** Compile-time stub of the real `android.test.mock.MockContext`. */
public abstract class MockContext extends android.content.Context {
  @Override public android.content.res.AssetManager getAssets() { return null;}
  @Override public android.content.res.Resources getResources() { return null;}
  @Override public android.content.pm.PackageManager getPackageManager() { return null;}
  @Override public android.content.ContentResolver getContentResolver() { return null;}
  @Override public android.os.Looper getMainLooper() { return null;}
  @Override public android.content.Context getApplicationContext() { return null;}
  @Override public void setTheme(int arg0) {}
  @Override public android.content.res.Resources.Theme getTheme() { return null;}
  @Override public java.lang.ClassLoader getClassLoader() { return null;}
  @Override public java.lang.String getPackageName() { return null;}
  @Override public android.content.pm.ApplicationInfo getApplicationInfo() { return null;}
  @Override public java.lang.String getPackageResourcePath() { return null;}
  @Override public java.lang.String getPackageCodePath() { return null;}
  @Override public android.content.SharedPreferences getSharedPreferences(java.lang.String arg0, int arg1) { return null;}
  @Override public boolean moveSharedPreferencesFrom(android.content.Context arg0, java.lang.String arg1) { return false;}
  @Override public boolean deleteSharedPreferences(java.lang.String arg0) { return false;}
  @Override public java.io.FileInputStream openFileInput(java.lang.String arg0) { return null;}
  @Override public java.io.FileOutputStream openFileOutput(java.lang.String arg0, int arg1) { return null;}
  @Override public boolean deleteFile(java.lang.String arg0) { return false;}
  @Override public java.io.File getFileStreamPath(java.lang.String arg0) { return null;}
  @Override public java.io.File getDataDir() { return null;}
  @Override public java.io.File getFilesDir() { return null;}
  @Override public java.io.File getNoBackupFilesDir() { return null;}
  @Override public java.io.File getExternalFilesDir(java.lang.String arg0) { return null;}
  @Override public java.io.File[] getExternalFilesDirs(java.lang.String arg0) { return null;}
  @Override public java.io.File getObbDir() { return null;}
  @Override public java.io.File[] getObbDirs() { return null;}
  @Override public java.io.File getCacheDir() { return null;}
  @Override public java.io.File getCodeCacheDir() { return null;}
  @Override public java.io.File getExternalCacheDir() { return null;}
  @Override public java.io.File[] getExternalCacheDirs() { return null;}
  @Override public java.io.File[] getExternalMediaDirs() { return null;}
  @Override public java.lang.String[] fileList() { return null;}
  @Override public java.io.File getDir(java.lang.String arg0, int arg1) { return null;}
  @Override public android.database.sqlite.SQLiteDatabase openOrCreateDatabase(java.lang.String arg0, int arg1, android.database.sqlite.SQLiteDatabase.CursorFactory arg2) { return null;}
  @Override public android.database.sqlite.SQLiteDatabase openOrCreateDatabase(java.lang.String arg0, int arg1, android.database.sqlite.SQLiteDatabase.CursorFactory arg2, android.database.DatabaseErrorHandler arg3) { return null;}
  @Override public boolean moveDatabaseFrom(android.content.Context arg0, java.lang.String arg1) { return false;}
  @Override public boolean deleteDatabase(java.lang.String arg0) { return false;}
  @Override public java.io.File getDatabasePath(java.lang.String arg0) { return null;}
  @Override public java.lang.String[] databaseList() { return null;}
  @Override public android.graphics.drawable.Drawable getWallpaper() { return null;}
  @Override public android.graphics.drawable.Drawable peekWallpaper() { return null;}
  @Override public int getWallpaperDesiredMinimumWidth() { return 0;}
  @Override public int getWallpaperDesiredMinimumHeight() { return 0;}
  @Override public void setWallpaper(android.graphics.Bitmap arg0) {}
  @Override public void setWallpaper(java.io.InputStream arg0) {}
  @Override public void clearWallpaper() {}
  @Override public void startActivity(android.content.Intent arg0) {}
  @Override public void startActivity(android.content.Intent arg0, android.os.Bundle arg1) {}
  @Override public void startActivities(android.content.Intent[] arg0) {}
  @Override public void startActivities(android.content.Intent[] arg0, android.os.Bundle arg1) {}
  @Override public void startIntentSender(android.content.IntentSender arg0, android.content.Intent arg1, int arg2, int arg3, int arg4) {}
  @Override public void startIntentSender(android.content.IntentSender arg0, android.content.Intent arg1, int arg2, int arg3, int arg4, android.os.Bundle arg5) {}
  @Override public void sendBroadcast(android.content.Intent arg0) {}
  @Override public void sendBroadcast(android.content.Intent arg0, java.lang.String arg1) {}
  @Override public void sendOrderedBroadcast(android.content.Intent arg0, java.lang.String arg1) {}
  @Override public void sendOrderedBroadcast(android.content.Intent arg0, java.lang.String arg1, android.content.BroadcastReceiver arg2, android.os.Handler arg3, int arg4, java.lang.String arg5, android.os.Bundle arg6) {}
  @Override public void sendBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1) {}
  @Override public void sendBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1, java.lang.String arg2) {}
  @Override public void sendOrderedBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1, java.lang.String arg2, android.content.BroadcastReceiver arg3, android.os.Handler arg4, int arg5, java.lang.String arg6, android.os.Bundle arg7) {}
  @Override public void sendStickyBroadcast(android.content.Intent arg0) {}
  @Override public void sendStickyOrderedBroadcast(android.content.Intent arg0, android.content.BroadcastReceiver arg1, android.os.Handler arg2, int arg3, java.lang.String arg4, android.os.Bundle arg5) {}
  @Override public void removeStickyBroadcast(android.content.Intent arg0) {}
  @Override public void sendStickyBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1) {}
  @Override public void sendStickyOrderedBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1, android.content.BroadcastReceiver arg2, android.os.Handler arg3, int arg4, java.lang.String arg5, android.os.Bundle arg6) {}
  @Override public void removeStickyBroadcastAsUser(android.content.Intent arg0, android.os.UserHandle arg1) {}
  @Override public android.content.Intent registerReceiver(android.content.BroadcastReceiver arg0, android.content.IntentFilter arg1) { return null;}
  @Override public android.content.Intent registerReceiver(android.content.BroadcastReceiver arg0, android.content.IntentFilter arg1, int arg2) { return null;}
  @Override public android.content.Intent registerReceiver(android.content.BroadcastReceiver arg0, android.content.IntentFilter arg1, java.lang.String arg2, android.os.Handler arg3) { return null;}
  @Override public android.content.Intent registerReceiver(android.content.BroadcastReceiver arg0, android.content.IntentFilter arg1, java.lang.String arg2, android.os.Handler arg3, int arg4) { return null;}
  @Override public void unregisterReceiver(android.content.BroadcastReceiver arg0) {}
  @Override public android.content.ComponentName startService(android.content.Intent arg0) { return null;}
  @Override public android.content.ComponentName startForegroundService(android.content.Intent arg0) { return null;}
  @Override public boolean stopService(android.content.Intent arg0) { return false;}
  @Override public boolean bindService(android.content.Intent arg0, android.content.ServiceConnection arg1, int arg2) { return false;}
  @Override public void unbindService(android.content.ServiceConnection arg0) {}
  @Override public boolean startInstrumentation(android.content.ComponentName arg0, java.lang.String arg1, android.os.Bundle arg2) { return false;}
  @Override public java.lang.Object getSystemService(java.lang.String arg0) { return null;}
  @Override public java.lang.String getSystemServiceName(java.lang.Class<?> arg0) { return null;}
  @Override public int checkPermission(java.lang.String arg0, int arg1, int arg2) { return 0;}
  @Override public int checkCallingPermission(java.lang.String arg0) { return 0;}
  @Override public int checkCallingOrSelfPermission(java.lang.String arg0) { return 0;}
  @Override public int checkSelfPermission(java.lang.String arg0) { return 0;}
  @Override public void enforcePermission(java.lang.String arg0, int arg1, int arg2, java.lang.String arg3) {}
  @Override public void enforceCallingPermission(java.lang.String arg0, java.lang.String arg1) {}
  @Override public void enforceCallingOrSelfPermission(java.lang.String arg0, java.lang.String arg1) {}
  @Override public void grantUriPermission(java.lang.String arg0, android.net.Uri arg1, int arg2) {}
  @Override public void revokeUriPermission(android.net.Uri arg0, int arg1) {}
  @Override public void revokeUriPermission(java.lang.String arg0, android.net.Uri arg1, int arg2) {}
  @Override public int checkUriPermission(android.net.Uri arg0, int arg1, int arg2, int arg3) { return 0;}
  @Override public int checkCallingUriPermission(android.net.Uri arg0, int arg1) { return 0;}
  @Override public int checkCallingOrSelfUriPermission(android.net.Uri arg0, int arg1) { return 0;}
  @Override public int checkUriPermission(android.net.Uri arg0, java.lang.String arg1, java.lang.String arg2, int arg3, int arg4, int arg5) { return 0;}
  @Override public void enforceUriPermission(android.net.Uri arg0, int arg1, int arg2, int arg3, java.lang.String arg4) {}
  @Override public void enforceCallingUriPermission(android.net.Uri arg0, int arg1, java.lang.String arg2) {}
  @Override public void enforceCallingOrSelfUriPermission(android.net.Uri arg0, int arg1, java.lang.String arg2) {}
  @Override public void enforceUriPermission(android.net.Uri arg0, java.lang.String arg1, java.lang.String arg2, int arg3, int arg4, int arg5, java.lang.String arg6) {}
  @Override public android.content.Context createPackageContext(java.lang.String arg0, int arg1) { return null;}
  @Override public android.content.Context createContextForSplit(java.lang.String arg0) { return null;}
  @Override public android.content.Context createConfigurationContext(android.content.res.Configuration arg0) { return null;}
  @Override public android.content.Context createDisplayContext(android.view.Display arg0) { return null;}
  @Override public android.content.Context createDeviceProtectedStorageContext() { return null;}
  @Override public boolean isDeviceProtectedStorage() { return false;}
}
