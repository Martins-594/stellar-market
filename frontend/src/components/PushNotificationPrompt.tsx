'use client';

import { useState, useEffect } from 'react';
import { Bell, X } from 'lucide-react';
import Button from './Button';

const API_URL = process.env.NEXT_PUBLIC_API_URL || 'http://localhost:5000/api/v1';

export default function PushNotificationPrompt() {
  const [showPrompt, setShowPrompt] = useState(false);
  const [permission, setPermission] = useState<NotificationPermission>('default');
  const [subscribeError, setSubscribeError] = useState<string | null>(null);

  useEffect(() => {
    // Check if notifications are supported
    if (!('Notification' in window) || !('serviceWorker' in navigator)) {
      return;
    }

    setPermission(Notification.permission);

    // Show prompt if permission hasn't been decided and user has interacted
    const hasInteracted = localStorage.getItem('stellarmarket-has-interacted');
    const promptDismissed = localStorage.getItem('stellarmarket-push-prompt-dismissed');

    if (
      Notification.permission === 'default' &&
      hasInteracted &&
      !promptDismissed
    ) {
      // Show prompt after a short delay
      const timer = setTimeout(() => setShowPrompt(true), 3000);
      return () => clearTimeout(timer);
    }
  }, []);

  const handleRequestPermission = async () => {
    try {
      const permission = await Notification.requestPermission();
      setPermission(permission);

      if (permission === 'granted') {
        const error = await subscribeToPushNotifications();
        if (error) {
          setSubscribeError(error);
        } else {
          setShowPrompt(false);
        }
      }
    } catch (error) {
      console.error('Error requesting notification permission:', error);
    }
  };

  const handleDismiss = () => {
    setShowPrompt(false);
    localStorage.setItem('stellarmarket-push-prompt-dismissed', 'true');
  };

  if (!showPrompt || permission !== 'default') {
    return null;
  }

  return (
    <div
      className="fixed bottom-4 right-4 z-50 max-w-sm bg-white dark:bg-gray-800 rounded-lg shadow-xl border border-gray-200 dark:border-gray-700 p-4"
      role="dialog"
      aria-labelledby="push-prompt-title"
      aria-describedby="push-prompt-description"
    >
      <div className="flex items-start gap-3">
        <div className="flex-shrink-0">
          <Bell className="w-6 h-6 text-stellar-blue" aria-hidden="true" />
        </div>
        <div className="flex-1">
          <h3
            id="push-prompt-title"
            className="font-semibold text-gray-900 dark:text-white mb-1"
          >
            Stay updated
          </h3>
          <p
            id="push-prompt-description"
            className="text-sm text-gray-600 dark:text-gray-400 mb-3"
          >
            Get notified about new job matches, applications, milestones, and messages.
          </p>
          <div className="flex gap-2">
            <Button
              variant="primary"
              size="sm"
              onClick={handleRequestPermission}
            >
              Enable notifications
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={handleDismiss}
            >
              Not now
            </Button>
          </div>
          {subscribeError && (
            <p className="mt-2 text-sm text-red-600 dark:text-red-400" role="alert">
              {subscribeError}
            </p>
          )}
        </div>
        <button
          onClick={handleDismiss}
          className="flex-shrink-0 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
          aria-label="Dismiss notification prompt"
        >
          <X className="w-5 h-5" />
        </button>
      </div>
    </div>
  );
}

async function subscribeToPushNotifications(): Promise<string | null> {
  try {
    const registration = await navigator.serviceWorker.ready;

    // Generate VAPID public key from environment
    const vapidPublicKey = process.env.NEXT_PUBLIC_VAPID_PUBLIC_KEY;
    if (!vapidPublicKey) {
      console.error('VAPID public key not configured');
      return 'Push notifications are not configured. Please contact support.';
    }

    const applicationServerKey = urlBase64ToUint8Array(vapidPublicKey);
    const subscription = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: applicationServerKey.buffer as ArrayBuffer,
    });

    const token = localStorage.getItem('stellarmarket_jwt');

    // Send subscription to backend using the correct API URL and auth header
    const response = await fetch(`${API_URL}/notifications/push/subscribe`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(subscription.toJSON()),
    });

    if (!response.ok) {
      console.error('Failed to subscribe to push notifications', response.status);
      return 'Failed to enable push notifications. Please try again.';
    }

    return null;
  } catch (error) {
    console.error('Error subscribing to push notifications:', error);
    return 'An error occurred while enabling push notifications.';
  }
}

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = '='.repeat((4 - (base64String.length % 4)) % 4);
  const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/');

  const rawData = window.atob(base64);
  const outputArray = new Uint8Array(rawData.length);

  for (let i = 0; i < rawData.length; ++i) {
    outputArray[i] = rawData.charCodeAt(i);
  }

  return outputArray;
}
