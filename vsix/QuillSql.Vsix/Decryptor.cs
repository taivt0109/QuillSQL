using System;
using System.Security.Cryptography;
using System.Text;
using Microsoft.Data.SqlClient;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Reads the definition of a module created WITH ENCRYPTION. SQL Server keeps it RC4-encrypted
    /// in sys.sysobjvalues, with a key derived from the database's family GUID and the object's ids;
    /// that table is only visible on the dedicated admin connection (DAC), which needs sysadmin.
    /// Nothing is changed on the server: the text is decrypted here.
    /// </summary>
    internal static class Decryptor
    {
        private const string Query = @"
SELECT CONVERT(binary(16), drs.family_guid), sov.objid, sov.subobjid, sov.imageval
FROM sys.sysobjvalues AS sov
CROSS JOIN sys.database_recovery_status AS drs
WHERE drs.database_id = DB_ID() AND sov.valclass = 1 AND sov.objid = OBJECT_ID(@name)
ORDER BY sov.subobjid;";

        /// <summary>The decrypted definition of `fullName`; throws with the reason when it cannot be read.</summary>
        public static string Decrypt(ConnectionTarget target, string fullName)
        {
            var dac = new SqlConnectionStringBuilder(target.ConnectionString);
            var server = dac.DataSource.StartsWith("tcp:", StringComparison.OrdinalIgnoreCase) ? dac.DataSource.Substring(4) : dac.DataSource;
            if (!server.StartsWith("admin:", StringComparison.OrdinalIgnoreCase))
            {
                dac.DataSource = "admin:" + server;
            }
            // The DAC accepts one connection at a time; never keep it open.
            dac.Pooling = false;
            dac.ApplicationName = "QuillSQL decrypt";
            using (var connection = new SqlConnection(dac.ConnectionString))
            {
                if (target.AccessToken != null)
                {
                    connection.AccessToken = target.AccessToken();
                }
                try
                {
                    connection.Open();
                }
                catch (SqlException ex)
                {
                    throw new InvalidOperationException("The dedicated admin connection to " + server + " could not be opened (" + ex.Message
                        + "). It needs a sysadmin login, and 'remote admin connections' enabled on the server when SSMS is on another computer.", ex);
                }
                using (var command = new SqlCommand(Query, connection) { CommandTimeout = 30 })
                {
                    command.Parameters.AddWithValue("@name", fullName);
                    using (var r = command.ExecuteReader())
                    {
                        if (!r.Read() || r.IsDBNull(3))
                        {
                            throw new InvalidOperationException(fullName + " has no encrypted definition to read.");
                        }
                        var family = (byte[])r.GetValue(0);
                        var objectId = r.GetInt32(1);
                        var subobjectId = Convert.ToInt16(r.GetValue(2));
                        var encrypted = (byte[])r.GetValue(3);
                        return Encoding.Unicode.GetString(Rc4(Key(family, objectId, subobjectId), encrypted));
                    }
                }
            }
        }

        /// <summary>SHA-1 of the family GUID, the object id and the sub-object id (both little-endian).</summary>
        internal static byte[] Key(byte[] family, int objectId, short subobjectId)
        {
            var data = new byte[family.Length + 6];
            Buffer.BlockCopy(family, 0, data, 0, family.Length);
            Buffer.BlockCopy(LittleEndian(BitConverter.GetBytes(objectId)), 0, data, family.Length, 4);
            Buffer.BlockCopy(LittleEndian(BitConverter.GetBytes(subobjectId)), 0, data, family.Length + 4, 2);
            using (var sha = SHA1.Create())
            {
                return sha.ComputeHash(data);
            }
        }

        private static byte[] LittleEndian(byte[] bytes)
        {
            if (!BitConverter.IsLittleEndian)
            {
                Array.Reverse(bytes);
            }
            return bytes;
        }

        internal static byte[] Rc4(byte[] key, byte[] data)
        {
            var s = new byte[256];
            for (var i = 0; i < 256; i++)
            {
                s[i] = (byte)i;
            }
            for (int i = 0, j = 0; i < 256; i++)
            {
                j = (j + s[i] + key[i % key.Length]) & 255;
                var t = s[i];
                s[i] = s[j];
                s[j] = t;
            }
            var output = new byte[data.Length];
            for (int n = 0, i = 0, j = 0; n < data.Length; n++)
            {
                i = (i + 1) & 255;
                j = (j + s[i]) & 255;
                var t = s[i];
                s[i] = s[j];
                s[j] = t;
                output[n] = (byte)(data[n] ^ s[(s[i] + s[j]) & 255]);
            }
            return output;
        }
    }
}
