# Copy to signing/signing.bzl (git-ignored) and put your own values there.
# The .mobileprovision files go next to it, in signing/.
IOS_BUNDLE_ID = "com.bitomule.alondra"
MAC_BUNDLE_ID = IOS_BUNDLE_ID + ".mac"
NOTIFICATION_SERVICE_BUNDLE_ID = IOS_BUNDLE_ID + ".NotificationService"
IOS_PROFILE = "Alondra_Development.mobileprovision"
NOTIFICATION_SERVICE_PROFILE = "Alondra_NotificationService_Development.mobileprovision"
