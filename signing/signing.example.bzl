# Copy to signing.bzl in your signing folder (ALONDRA_SIGNING_DIR, default ~/.alondra/signing)
# and put your own values there. Your .mobileprovision and .provisionprofile files go in the
# same folder; none of them live in this repository.
IOS_BUNDLE_ID = "com.example.alondra"
NOTIFICATION_SERVICE_BUNDLE_ID = IOS_BUNDLE_ID + ".NotificationService"
IOS_PROFILE = "Alondra_Development.mobileprovision"
NOTIFICATION_SERVICE_PROFILE = "Alondra_NotificationService_Development.mobileprovision"
LIVE_ACTIVITY_BUNDLE_ID = IOS_BUNDLE_ID + ".LiveActivity"
LIVE_ACTIVITY_PROFILE = "Alondra_LiveActivity_Development.mobileprovision"
