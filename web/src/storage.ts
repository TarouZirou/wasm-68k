/** IndexedDB上のUint8Array専用store。ROM・SRAM・状態を同じ経路で扱う。 */
export class BrowserBinaryStore {
  private serializedWrite = Promise.resolve();

  constructor(
    private readonly databaseName: string,
    private readonly storeName: string,
  ) {}

  async put(key: string, value: Uint8Array): Promise<void> {
    const database = await this.open();
    try {
      await new Promise<void>((resolve, reject) => {
        const transaction = database.transaction(this.storeName, "readwrite");
        transaction.objectStore(this.storeName).put(value, key);
        transaction.oncomplete = () => resolve();
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      database.close();
    }
  }

  async get(key: string): Promise<Uint8Array | undefined> {
    const database = await this.open();
    try {
      return await new Promise<Uint8Array | undefined>((resolve, reject) => {
        const request = database.transaction(this.storeName).objectStore(this.storeName).get(key);
        request.onsuccess = () => resolve(request.result as Uint8Array | undefined);
        request.onerror = () => reject(request.error);
      });
    } finally {
      database.close();
    }
  }

  async delete(key: string): Promise<void> {
    const database = await this.open();
    try {
      await new Promise<void>((resolve, reject) => {
        const transaction = database.transaction(this.storeName, "readwrite");
        transaction.objectStore(this.storeName).delete(key);
        transaction.oncomplete = () => resolve();
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      database.close();
    }
  }

  /** 頻繁なUI変更を発火順に確定し、古い設定が後から上書きする競合を防ぐ。 */
  putSerialized(key: string, value: Uint8Array): Promise<void> {
    const write = this.serializedWrite
      .catch(() => undefined)
      .then(() => this.put(key, value));
    this.serializedWrite = write;
    return write;
  }

  private open(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
      const request = indexedDB.open(this.databaseName, 1);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains(this.storeName)) {
          request.result.createObjectStore(this.storeName);
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  }
}
