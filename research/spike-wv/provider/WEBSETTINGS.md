# `android.webkit.WebSettings` abstract methods (API 34, from `javap` over the
# SDK's `platforms/android-34/android.jar`).
#
# `WebSettings` is *public* API, so the provider is compiled against android.jar
# for it (unlike the hidden `WebViewProvider` family, which comes from the
# image's dex, `METHODS.md`). One line per abstract method:
#
#   name(paramType,paramType)returnType
#
# `tools/gen-webview-provider.py` turns this into `WebSettingsChromium`: the
# methods in its IMPLEMENTED map forward to the provider's real code; every
# other one throws a structured `UnsupportedOperationException` naming it
# (ADR-0031, 0 false passes). Regenerate from:
#   javap -cp $ANDROID_JAR android.webkit.WebSettings

setSupportZoom(boolean)void
supportZoom()boolean
setMediaPlaybackRequiresUserGesture(boolean)void
getMediaPlaybackRequiresUserGesture()boolean
setBuiltInZoomControls(boolean)void
getBuiltInZoomControls()boolean
setDisplayZoomControls(boolean)void
getDisplayZoomControls()boolean
setAllowFileAccess(boolean)void
getAllowFileAccess()boolean
setAllowContentAccess(boolean)void
getAllowContentAccess()boolean
setLoadWithOverviewMode(boolean)void
getLoadWithOverviewMode()boolean
setEnableSmoothTransition(boolean)void
enableSmoothTransition()boolean
setSaveFormData(boolean)void
getSaveFormData()boolean
setSavePassword(boolean)void
getSavePassword()boolean
setTextZoom(int)void
getTextZoom()int
setDefaultZoom(android.webkit.WebSettings.ZoomDensity)void
getDefaultZoom()android.webkit.WebSettings.ZoomDensity
setLightTouchEnabled(boolean)void
getLightTouchEnabled()boolean
setUseWideViewPort(boolean)void
getUseWideViewPort()boolean
setSupportMultipleWindows(boolean)void
supportMultipleWindows()boolean
setLayoutAlgorithm(android.webkit.WebSettings.LayoutAlgorithm)void
getLayoutAlgorithm()android.webkit.WebSettings.LayoutAlgorithm
setStandardFontFamily(java.lang.String)void
getStandardFontFamily()java.lang.String
setFixedFontFamily(java.lang.String)void
getFixedFontFamily()java.lang.String
setSansSerifFontFamily(java.lang.String)void
getSansSerifFontFamily()java.lang.String
setSerifFontFamily(java.lang.String)void
getSerifFontFamily()java.lang.String
setCursiveFontFamily(java.lang.String)void
getCursiveFontFamily()java.lang.String
setFantasyFontFamily(java.lang.String)void
getFantasyFontFamily()java.lang.String
setMinimumFontSize(int)void
getMinimumFontSize()int
setMinimumLogicalFontSize(int)void
getMinimumLogicalFontSize()int
setDefaultFontSize(int)void
getDefaultFontSize()int
setDefaultFixedFontSize(int)void
getDefaultFixedFontSize()int
setLoadsImagesAutomatically(boolean)void
getLoadsImagesAutomatically()boolean
setBlockNetworkImage(boolean)void
getBlockNetworkImage()boolean
setBlockNetworkLoads(boolean)void
getBlockNetworkLoads()boolean
setJavaScriptEnabled(boolean)void
setAllowUniversalAccessFromFileURLs(boolean)void
setAllowFileAccessFromFileURLs(boolean)void
setPluginState(android.webkit.WebSettings.PluginState)void
setDatabasePath(java.lang.String)void
setGeolocationDatabasePath(java.lang.String)void
setDatabaseEnabled(boolean)void
setDomStorageEnabled(boolean)void
getDomStorageEnabled()boolean
getDatabasePath()java.lang.String
getDatabaseEnabled()boolean
setGeolocationEnabled(boolean)void
getJavaScriptEnabled()boolean
getAllowUniversalAccessFromFileURLs()boolean
getAllowFileAccessFromFileURLs()boolean
getPluginState()android.webkit.WebSettings.PluginState
setJavaScriptCanOpenWindowsAutomatically(boolean)void
getJavaScriptCanOpenWindowsAutomatically()boolean
setDefaultTextEncodingName(java.lang.String)void
getDefaultTextEncodingName()java.lang.String
setUserAgentString(java.lang.String)void
getUserAgentString()java.lang.String
setNeedInitialFocus(boolean)void
setRenderPriority(android.webkit.WebSettings.RenderPriority)void
setCacheMode(int)void
getCacheMode()int
setMixedContentMode(int)void
getMixedContentMode()int
setOffscreenPreRaster(boolean)void
getOffscreenPreRaster()boolean
setSafeBrowsingEnabled(boolean)void
getSafeBrowsingEnabled()boolean
setDisabledActionModeMenuItems(int)void
getDisabledActionModeMenuItems()int
