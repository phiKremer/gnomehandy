import Gio from 'gi://Gio';

const SERVICE_NAME = 'de.pkremer.GnomeHandy';
const OBJECT_PATH = '/de/pkremer/GnomeHandy';
const INTERFACE_NAME = 'de.pkremer.GnomeHandy';

export function callRust(method, parameters = null) {
    return new Promise((resolve, reject) => {
        Gio.DBus.session.call(
            SERVICE_NAME,
            OBJECT_PATH,
            INTERFACE_NAME,
            method,
            parameters,
            null,
            Gio.DBusCallFlags.NONE,
            3000,
            null,
            (connection, result) => {
                try {
                    resolve(connection.call_finish(result));
                } catch (error) {
                    reject(error);
                }
            },
        );
    });
}

export function subscribeToRustSignal(signalName, callback) {
    return Gio.DBus.session.signal_subscribe(
        SERVICE_NAME,
        INTERFACE_NAME,
        signalName,
        OBJECT_PATH,
        null,
        Gio.DBusSignalFlags.NONE,
        callback,
    );
}

export function unsubscribeFromRustSignal(subscriptionId) {
    if (subscriptionId)
        Gio.DBus.session.signal_unsubscribe(subscriptionId);
}
